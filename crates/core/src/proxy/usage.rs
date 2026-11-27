use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_prompt_tokens: i64,
    pub cache_write_tokens: i64,
}

impl Usage {
    /// Extract usage from a non-stream JSON response body. Compatible with
    /// OpenAI (`prompt_tokens`/`completion_tokens`/`prompt_tokens_details.cached_tokens`),
    /// OpenAI Responses (`input_tokens_details.cached_tokens`/`input_token_details.cached_tokens`),
    /// Anthropic (`input_tokens`/`output_tokens`/`cache_read_input_tokens`/`cache_creation_input_tokens`),
    /// DeepSeek (`prompt_cache_hit_tokens`), and Gemini (`cachedContentTokenCount`).
    pub fn from_json(bytes: &[u8]) -> Self {
        let v: Value = match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        let usage = v
            .get("usage")
            .or_else(|| v.get("usageMetadata"))
            .cloned()
            .unwrap_or(Value::Null);
        Self::from_usage_value(&usage)
    }

    pub fn from_usage_value(usage: &Value) -> Self {
        if usage.is_null() {
            return Self::default();
        }
        let raw_prompt = usage.get("prompt_tokens").and_then(|t| t.as_i64());
        let anthropic_input = usage.get("input_tokens").and_then(|t| t.as_i64());
        let completion = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .or_else(|| usage.get("candidatesTokenCount"))
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
            })
            .or_else(|| {
                usage
                    .get("input_token_details")
                    .and_then(|d| d.get("cached_tokens"))
            })
            .or_else(|| usage.get("cache_read_input_tokens"))
            .or_else(|| usage.get("prompt_cache_hit_tokens"))
            .or_else(|| usage.get("cachedContentTokenCount"))
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(|t| t.as_i64())
            .unwrap_or(0);

        let prompt = if let Some(p) = raw_prompt {
            p
        } else if let Some(inp) = anthropic_input {
            // In Anthropic Messages API, `input_tokens` only counts new/uncached tokens.
            // Total prompt tokens processed is input_tokens + cached + cache_write.
            inp + cached + cache_write
        } else {
            usage
                .get("promptTokenCount")
                .and_then(|t| t.as_i64())
                .unwrap_or(0)
        };

        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_prompt_tokens: cached,
            cache_write_tokens: cache_write,
        }
    }

    /// Extract usage from the tail of an SSE stream.
    ///
    /// Parses each `data: <json>` line (the `[DONE]` sentinel is skipped) and
    /// merges usage information:
    ///   - `prompt_tokens` or `input_tokens` (in a usage block) updates the
    ///     prompt count;
    ///   - `completion_tokens` or `output_tokens` (in a usage block) updates
    ///     the completion count;
    ///   - `prompt_tokens_details.cached_tokens`, `input_tokens_details.cached_tokens`,
    ///     or `cache_read_input_tokens` updates cached count;
    ///   - `cache_creation_input_tokens` updates cache write count.
    ///
    /// Note: in Anthropic's streaming protocol, `message_delta` events carry a
    /// cumulative `output_tokens` (the running total for the response), so we
    /// take the last seen value — which `take_last` semantics provide by
    /// always overwriting when a value is present.
    ///
    /// The rules above cover OpenAI (`chat`/`responses`) and Anthropic
    /// (`messages`) streams.
    pub fn from_sse_tail(tail: &[u8]) -> Self {
        let text = String::from_utf8_lossy(tail);
        let mut raw_prompt: Option<i64> = None;
        let mut anthropic_input: Option<i64> = None;
        let mut completion: Option<i64> = None;
        let mut cached: Option<i64> = None;
        let mut cache_write: Option<i64> = None;

        for line in text.lines() {
            let trimmed = line.trim_start();
            let payload = if let Some(rest) = trimmed.strip_prefix("data:") {
                rest.trim_start()
            } else {
                continue;
            };
            if payload == "[DONE]" {
                continue;
            }
            let v: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Usage may live at the top level of the event (OpenAI final
            // chunk with include_usage), inside `usage`, or — for Anthropic's
            // `message_start` — nested under `message.usage`. `message_delta`
            // carries `usage` directly.
            let usage = v
                .get("usage")
                .cloned()
                .or_else(|| v.get("usageMetadata").cloned())
                .or_else(|| v.get("message").and_then(|m| m.get("usage")).cloned())
                .or_else(|| v.get("response").and_then(|m| m.get("usage")).cloned())
                .unwrap_or(Value::Null);
            if usage.is_null() {
                continue;
            }
            if let Some(p) = usage
                .get("prompt_tokens")
                .or_else(|| usage.get("promptTokenCount"))
                .and_then(|t| t.as_i64())
            {
                raw_prompt = Some(p);
            }
            if let Some(inp) = usage.get("input_tokens").and_then(|t| t.as_i64()) {
                anthropic_input = Some(inp);
            }
            if let Some(c) = usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .or_else(|| usage.get("candidatesTokenCount"))
                .and_then(|t| t.as_i64())
            {
                // message_delta's output_tokens is cumulative → take last.
                completion = Some(c);
            }
            if let Some(ca) = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .or_else(|| {
                    usage
                        .get("input_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                })
                .or_else(|| {
                    usage
                        .get("input_token_details")
                        .and_then(|d| d.get("cached_tokens"))
                })
                .or_else(|| usage.get("cache_read_input_tokens"))
                .or_else(|| usage.get("prompt_cache_hit_tokens"))
                .or_else(|| usage.get("cachedContentTokenCount"))
                .and_then(|t| t.as_i64())
            {
                cached = Some(ca);
            }
            if let Some(w) = usage
                .get("cache_creation_input_tokens")
                .and_then(|t| t.as_i64())
            {
                cache_write = Some(w);
            }
        }

        let prompt = if let Some(p) = raw_prompt {
            p
        } else if let Some(inp) = anthropic_input {
            inp + cached.unwrap_or(0) + cache_write.unwrap_or(0)
        } else {
            0
        };

        Self {
            prompt_tokens: prompt,
            completion_tokens: completion.unwrap_or(0),
            cached_prompt_tokens: cached.unwrap_or(0),
            cache_write_tokens: cache_write.unwrap_or(0),
        }
    }

    /// Extract usage from combined head and tail buffers of an SSE stream.
    /// This ensures head-emitted usage (such as Anthropic's `message_start` containing
    /// prompt and cache tokens) is not lost when stream responses exceed the tail buffer capacity.
    pub fn from_sse_buffers(head: &[u8], tail: &[u8]) -> Self {
        let u_head = Self::from_sse_tail(head);
        let u_tail = Self::from_sse_tail(tail);

        Self {
            prompt_tokens: if u_tail.prompt_tokens > 0 {
                u_tail.prompt_tokens
            } else {
                u_head.prompt_tokens
            },
            completion_tokens: if u_tail.completion_tokens > 0 {
                u_tail.completion_tokens
            } else {
                u_head.completion_tokens
            },
            cached_prompt_tokens: if u_tail.cached_prompt_tokens > 0 {
                u_tail.cached_prompt_tokens
            } else {
                u_head.cached_prompt_tokens
            },
            cache_write_tokens: if u_tail.cache_write_tokens > 0 {
                u_tail.cache_write_tokens
            } else {
                u_head.cache_write_tokens
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_sse_tail_openai_with_usage_chunk() {
        let tail = b"data: {\"choices\":[{\"delta\":{\"content\":\"o\"}}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":1}}\n\n\
data: [DONE]\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 7);
        assert_eq!(u.completion_tokens, 1);
    }

    #[test]
    fn from_sse_tail_anthropic_cumulative_output() {
        let tail = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 12);
        // last seen cumulative output_tokens wins
        assert_eq!(u.completion_tokens, 5);
    }

    #[test]
    fn from_sse_tail_no_usage_returns_zero() {
        let tail = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.cached_prompt_tokens, 0);
        assert_eq!(u.cache_write_tokens, 0);
    }

    #[test]
    fn from_json_anthropic_cache_tokens() {
        let json_body = br#"{
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 10
            }
        }"#;
        let u = Usage::from_json(json_body);
        // Total prompt tokens = uncached (100) + cache read (80) + cache write (10) = 190
        assert_eq!(u.prompt_tokens, 190);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.cached_prompt_tokens, 80);
        assert_eq!(u.cache_write_tokens, 10);
    }

    #[test]
    fn from_json_responses_api_plural_tokens_details() {
        let json_body = br#"{
            "usage": {
                "prompt_tokens": 250,
                "completion_tokens": 60,
                "input_tokens_details": {
                    "cached_tokens": 180
                }
            }
        }"#;
        let u = Usage::from_json(json_body);
        assert_eq!(u.prompt_tokens, 250);
        assert_eq!(u.completion_tokens, 60);
        assert_eq!(u.cached_prompt_tokens, 180);
        assert_eq!(u.cache_write_tokens, 0);
    }

    #[test]
    fn from_sse_buffers_preserves_head_prompt_tokens() {
        let head = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":50,\"cache_read_input_tokens\":2000}}}\n\n";
        let tail = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":120}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let u = Usage::from_sse_buffers(head, tail);
        assert_eq!(u.prompt_tokens, 2050); // 50 uncached + 2000 cache read
        assert_eq!(u.completion_tokens, 120);
        assert_eq!(u.cached_prompt_tokens, 2000);
    }

    #[test]
    fn from_json_openai_cached_tokens() {
        let json_body = br#"{
            "usage": {
                "prompt_tokens": 200,
                "completion_tokens": 80,
                "prompt_tokens_details": {
                    "cached_tokens": 150
                }
            }
        }"#;
        let u = Usage::from_json(json_body);
        assert_eq!(u.prompt_tokens, 200);
        assert_eq!(u.completion_tokens, 80);
        assert_eq!(u.cached_prompt_tokens, 150);
        assert_eq!(u.cache_write_tokens, 0);
    }

    #[test]
    fn from_sse_tail_openai_cached_tokens() {
        let tail = b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":90}}}\n\ndata: [DONE]\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 120);
        assert_eq!(u.completion_tokens, 30);
        assert_eq!(u.cached_prompt_tokens, 90);
        assert_eq!(u.cache_write_tokens, 0);
    }
}
