//! Robustness, timeout management, retry-after backoff, and stream interruption handling.

use axum::body::Bytes;
use axum::http::HeaderMap;
use std::time::Duration;

use crate::models::ProviderProtocol;

/// Tiered timeout configuration for the proxy forwarding pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutConfig {
    /// Connect timeout for establishing TCP/TLS connections to upstreams.
    pub connect_timeout: Duration,
    /// Time To First Token (TTFT) timeout: duration to wait before receiving the first semantic event.
    pub ttft_timeout: Duration,
    /// Chunk-to-chunk idle timeout during an active response stream.
    pub idle_timeout: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            ttft_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(15),
        }
    }
}

/// Parse `Retry-After` (seconds or HTTP-date) or `retry-after-ms` from response headers.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    // 1. Check `retry-after` header
    if let Some(val) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        // Try parsing integer seconds
        if let Ok(secs) = trimmed.parse::<u64>() {
            return Some(Duration::from_secs(secs));
        }
        // Try parsing HTTP-date (RFC 2822 / RFC 7231 / RFC 1123)
        if let Ok(target_time) =
            time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc2822)
        {
            let now = time::OffsetDateTime::now_utc();
            if target_time > now {
                let diff = target_time - now;
                return Some(Duration::from_secs(diff.whole_seconds().max(0) as u64));
            }
            return Some(Duration::ZERO);
        }
    }

    // 2. Check `retry-after-ms` header
    if let Some(val) = headers.get("retry-after-ms").and_then(|v| v.to_str().ok()) {
        if let Ok(ms) = val.trim().parse::<u64>() {
            return Some(Duration::from_millis(ms));
        }
    }

    None
}

/// The decision produced by the `SemanticDetector` when evaluating response chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitDecision {
    /// Still waiting for the first semantic chunk (keep buffering)
    Buffering,
    /// First semantic chunk arrived (contains actual text delta, reasoning, or tool call start)
    Commit { has_tool_call: bool },
    /// Detected an upstream error payload prior to any valid semantic event
    Error(String),
}

/// Inspects response bodies and streaming chunks to identify semantic commit points and error bodies.
pub struct SemanticDetector;

impl SemanticDetector {
    /// Inspect non-streaming response body: returns Some(error_message) if it's an error payload.
    pub fn is_error_payload(bytes: &[u8]) -> Option<String> {
        let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return None;
        };
        // Check for OpenAI/Anthropic/Responses root "error" object
        if let Some(err) = val.get("error") {
            if let Some(msg) = err.get("message").and_then(|m| m.as_str()) {
                return Some(msg.to_string());
            }
            if let Some(s) = err.as_str() {
                return Some(s.to_string());
            }
            return Some(err.to_string());
        }
        // Check for Anthropic root type="error"
        if val.get("type").and_then(|t| t.as_str()) == Some("error") {
            if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
                return Some(msg.to_string());
            }
            return Some(val.to_string());
        }
        None
    }

    /// Inspect a streaming chunk (or accumulated buffer) for commit events or error frames.
    pub fn inspect_chunk(buf: &[u8], proto: crate::models::ProviderProtocol) -> CommitDecision {
        use crate::models::ProviderProtocol;
        let text = String::from_utf8_lossy(buf);
        let mut has_tool_call = false;
        let mut has_semantic = false;
        let mut current_event: Option<String> = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                current_event = None;
                continue;
            }

            if let Some(ev) = line.strip_prefix("event:") {
                let ev_name = ev.trim();
                current_event = Some(ev_name.to_string());
                if ev_name == "error" || ev_name == "response.failed" {
                    return CommitDecision::Error("upstream stream error event".to_string());
                }
                continue;
            }

            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }

                if current_event.as_deref() == Some("error")
                    || current_event.as_deref() == Some("response.failed")
                {
                    return CommitDecision::Error(data.to_string());
                }

                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(err_msg) = Self::is_error_payload(data.as_bytes()) {
                        return CommitDecision::Error(err_msg);
                    }

                    match proto {
                        ProviderProtocol::Chat => {
                            if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
                                for choice in choices {
                                    if let Some(delta) = choice.get("delta") {
                                        if let Some(content) =
                                            delta.get("content").and_then(|c| c.as_str())
                                        {
                                            if !content.is_empty() {
                                                has_semantic = true;
                                            }
                                        }
                                        if let Some(reasoning) =
                                            delta.get("reasoning_content").and_then(|r| r.as_str())
                                        {
                                            if !reasoning.is_empty() {
                                                has_semantic = true;
                                            }
                                        }
                                        if let Some(tools) =
                                            delta.get("tool_calls").and_then(|t| t.as_array())
                                        {
                                            if !tools.is_empty() {
                                                has_semantic = true;
                                                has_tool_call = true;
                                            }
                                        }
                                        if delta.get("function_call").is_some() {
                                            has_semantic = true;
                                            has_tool_call = true;
                                        }
                                    }
                                }
                            }
                        }
                        ProviderProtocol::Messages => {
                            let type_str = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if type_str == "content_block_delta" {
                                if let Some(delta) = val.get("delta") {
                                    let delta_type =
                                        delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                    if delta_type == "text_delta" || delta_type == "thinking_delta"
                                    {
                                        has_semantic = true;
                                    } else if delta_type == "input_json_delta" {
                                        has_semantic = true;
                                        has_tool_call = true;
                                    }
                                }
                            } else if type_str == "content_block_start" {
                                if let Some(block) = val.get("content_block") {
                                    let block_type =
                                        block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                    if block_type == "tool_use" {
                                        has_semantic = true;
                                        has_tool_call = true;
                                    } else if block_type == "text" || block_type == "thinking" {
                                        has_semantic = true;
                                    }
                                }
                            }
                        }
                        ProviderProtocol::Responses => {
                            let ev_type = current_event.as_deref().unwrap_or_else(|| {
                                val.get("type").and_then(|t| t.as_str()).unwrap_or("")
                            });
                            if ev_type == "response.output_item.added" {
                                if let Some(item) =
                                    val.get("item").or_else(|| val.get("output_item"))
                                {
                                    let item_type =
                                        item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                    if item_type == "function_call" {
                                        has_semantic = true;
                                        has_tool_call = true;
                                    } else {
                                        has_semantic = true;
                                    }
                                }
                            } else if ev_type.ends_with(".delta") || ev_type.contains("delta") {
                                has_semantic = true;
                                if ev_type.contains("function_call") {
                                    has_tool_call = true;
                                }
                            }
                        }
                    }
                }
            }
        }

        if has_semantic {
            CommitDecision::Commit { has_tool_call }
        } else {
            CommitDecision::Buffering
        }
    }
}

/// Synthesize a protocol-native stream interruption event.
///
/// Sent when an active stream is interrupted post-commit (e.g. upstream drop or chunk idle timeout).
/// `retryable` is true if only text or reasoning tokens have been emitted, false if tool calls were started.
pub fn synthesize_stream_interrupted(
    proto: ProviderProtocol,
    req_id: &str,
    has_tool_call: bool,
) -> Bytes {
    let retryable = !has_tool_call;
    match proto {
        ProviderProtocol::Chat => {
            let json = serde_json::json!({
                "error": {
                    "code": "xfade_stream_interrupted",
                    "message": "upstream connection lost mid-stream",
                    "type": "xfade_stream_interrupted",
                    "request_id": req_id,
                    "retryable": retryable
                }
            });
            Bytes::from(format!("data: {json}\n\ndata: [DONE]\n\n"))
        }
        ProviderProtocol::Messages => {
            let json = serde_json::json!({
                "type": "error",
                "error": {
                    "type": "xfade_stream_interrupted",
                    "message": "upstream connection lost mid-stream",
                    "request_id": req_id,
                    "retryable": retryable
                }
            });
            Bytes::from(format!("event: error\ndata: {json}\n\n"))
        }
        ProviderProtocol::Responses => {
            let json = serde_json::json!({
                "type": "response.failed",
                "response": {
                    "id": req_id,
                    "status": "failed",
                    "error": {
                        "code": "xfade_stream_interrupted",
                        "message": "upstream connection lost mid-stream",
                        "retryable": retryable
                    }
                }
            });
            Bytes::from(format!("event: response.failed\ndata: {json}\n\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use std::time::Duration;

    #[test]
    fn test_timeout_config_defaults() {
        let cfg = TimeoutConfig::default();
        assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
        assert_eq!(cfg.ttft_timeout, Duration::from_secs(30));
        assert_eq!(cfg.idle_timeout, Duration::from_secs(15));
    }

    #[test]
    fn test_parse_retry_after_seconds_and_millis() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", "2".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(2)));

        let mut h_ms = HeaderMap::new();
        h_ms.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(parse_retry_after(&h_ms), Some(Duration::from_millis(1500)));

        let h_empty = HeaderMap::new();
        assert_eq!(parse_retry_after(&h_empty), None);

        let mut h_invalid = HeaderMap::new();
        h_invalid.insert("retry-after", "not-a-number".parse().unwrap());
        assert_eq!(parse_retry_after(&h_invalid), None);
    }

    #[test]
    fn test_semantic_detector_text_thinking_and_tool_call() {
        use crate::models::ProviderProtocol;

        // Chat content delta -> Commit { has_tool_call: false }
        let chat_text = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n";
        assert!(matches!(
            SemanticDetector::inspect_chunk(chat_text, ProviderProtocol::Chat),
            CommitDecision::Commit {
                has_tool_call: false
            }
        ));

        // Chat reasoning delta -> Commit { has_tool_call: false }
        let chat_reasoning =
            b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n";
        assert!(matches!(
            SemanticDetector::inspect_chunk(chat_reasoning, ProviderProtocol::Chat),
            CommitDecision::Commit {
                has_tool_call: false
            }
        ));

        // Chat tool_calls delta -> Commit { has_tool_call: true }
        let chat_tool =
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\"}]}}]}\n\n";
        assert!(matches!(
            SemanticDetector::inspect_chunk(chat_tool, ProviderProtocol::Chat),
            CommitDecision::Commit {
                has_tool_call: true
            }
        ));

        // Messages thinking delta -> Commit { has_tool_call: false }
        let msg_thinking = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n";
        assert!(matches!(
            SemanticDetector::inspect_chunk(msg_thinking, ProviderProtocol::Messages),
            CommitDecision::Commit {
                has_tool_call: false
            }
        ));

        // Messages tool_use -> Commit { has_tool_call: true }
        let msg_tool = b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool_1\"}}\n\n";
        assert!(matches!(
            SemanticDetector::inspect_chunk(msg_tool, ProviderProtocol::Messages),
            CommitDecision::Commit {
                has_tool_call: true
            }
        ));

        // Upstream 200 error chunk -> Error
        let err_chunk = b"data: {\"error\":{\"message\":\"quota exceeded\",\"code\":40001}}\n\n";
        assert!(matches!(
            SemanticDetector::inspect_chunk(err_chunk, ProviderProtocol::Chat),
            CommitDecision::Error(_)
        ));
    }

    #[test]
    fn test_is_error_payload_json() {
        let err_json = br#"{"error":{"message":"balance exhausted"}}"#;
        assert!(SemanticDetector::is_error_payload(err_json).is_some());

        let ok_json = br#"{"choices":[{"message":{"content":"Hi"}}]}"#;
        assert!(SemanticDetector::is_error_payload(ok_json).is_none());
    }

    #[test]
    fn test_synthesize_stream_interrupted_wire_formats() {
        use crate::models::ProviderProtocol;

        // 1. Chat text-only (retryable: true)
        let b_chat = synthesize_stream_interrupted(ProviderProtocol::Chat, "req-123", false);
        let s_chat = String::from_utf8_lossy(&b_chat);
        assert!(s_chat.contains(r#""code":"xfade_stream_interrupted""#));
        assert!(s_chat.contains(r#""request_id":"req-123""#));
        assert!(s_chat.contains(r#""retryable":true"#));
        assert!(s_chat.ends_with("data: [DONE]\n\n"));

        // 2. Chat with tool_call (retryable: false)
        let b_chat_tool = synthesize_stream_interrupted(ProviderProtocol::Chat, "req-123", true);
        let s_chat_tool = String::from_utf8_lossy(&b_chat_tool);
        assert!(s_chat_tool.contains(r#""retryable":false"#));

        // 3. Messages text-only (event: error)
        let b_msg = synthesize_stream_interrupted(ProviderProtocol::Messages, "req-456", false);
        let s_msg = String::from_utf8_lossy(&b_msg);
        assert!(s_msg.starts_with("event: error\n"));
        assert!(s_msg.contains(r#""type":"xfade_stream_interrupted""#));
        assert!(s_msg.contains(r#""retryable":true"#));

        // 4. Responses with tool_call (event: response.failed)
        let b_resp = synthesize_stream_interrupted(ProviderProtocol::Responses, "req-789", true);
        let s_resp = String::from_utf8_lossy(&b_resp);
        assert!(s_resp.contains("response.failed"));
        assert!(s_resp.contains(r#""retryable":false"#));
    }
}
