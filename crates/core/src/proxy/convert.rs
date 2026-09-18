//! Protocol conversion: Anthropic messages → OpenAI chat/completions, and
//! OpenAI responses → OpenAI chat/completions.
//!
//! This module contains:
//! - Request-side conversion (Anthropic → OpenAI), see `request_anthropic_to_openai`.
//! - Response-side conversion (OpenAI → Anthropic), non-stream + streaming
//!   state machine, see `response_openai_to_anthropic` and
//!   `stream_openai_to_anthropic`. See spec §3 response side.
//! - Responses↔Chat conversion for `/v1/responses` clients (Codex) against
//!   chat-completions-only upstreams, see `request_responses_to_chat`,
//!   `response_chat_to_responses` and `stream_chat_to_responses`.

use crate::error::{CoreError, Result};
use axum::body::Bytes;
use futures::Stream;
use serde_json::{json, Map, Value};

/// Convert an Anthropic `/v1/messages` request body into an OpenAI
/// `/v1/chat/completions` request body.
///
/// Behavior (see spec §3 request side):
/// - `system` (string or content-block array) → first `{role:"system", content:<joined text>}` message
/// - `messages[].content` blocks:
///   - text → content string (single block) or array part (multi block)
///   - image → `{type:"image_url", image_url:{url:<data URI>}}` part (passthrough)
///   - tool_use (assistant) → `tool_calls:[{id, type:"function", function:{name, arguments:<JSON string>}}]`
///   - tool_result (user) → split into `{role:"tool", tool_call_id, content}` message
/// - `tools[]` (input_schema) → `tools[]` (`{type:"function", function:{name, description, parameters}}`)
/// - `tool_choice`: `{type:"auto"}`→`"auto"`; `{type:"any"}`→`"required"`; `{type:"tool", name}`→`{type:"function", function:{name}}`
/// - `max_tokens`/`temperature`/`top_p`/`stream` passthrough
/// - `model`: override if provided, else passthrough
/// - thinking blocks are dropped
pub fn request_anthropic_to_openai(body: &[u8], model_override: Option<&str>) -> Result<Bytes> {
    let input: Value = serde_json::from_slice(body)?;
    let obj = input
        .as_object()
        .ok_or_else(|| CoreError::Proxy("anthropic request body is not a JSON object".into()))?;

    let mut out = Map::new();

    // model: override or passthrough
    let model = model_override
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            obj.get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
    if let Some(m) = model {
        out.insert("model".into(), Value::String(m));
    }

    // passthrough fields
    for key in ["max_tokens", "temperature", "top_p", "stream"] {
        if let Some(v) = obj.get(key) {
            out.insert(key.into(), v.clone());
        }
    }

    // messages: build output messages array, prepending system if present
    let mut out_messages: Vec<Value> = Vec::new();

    if let Some(system) = obj.get("system") {
        let system_text = system_to_text(system);
        if !system_text.is_empty() {
            out_messages.push(json!({"role": "system", "content": system_text}));
        }
    }

    let in_messages = obj
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CoreError::Proxy("anthropic request missing messages array".into()))?;

    for msg in in_messages {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CoreError::Proxy("message missing role".into()))?;
        let content = msg.get("content");
        match role {
            "assistant" => {
                out_messages.push(convert_assistant_message(role, content)?);
            }
            "user" => {
                convert_user_message(content, &mut out_messages)?;
            }
            _ => {
                // other roles: passthrough content as-is if string, else best-effort
                let content_val = content.cloned().unwrap_or(Value::Null);
                out_messages.push(json!({"role": role, "content": content_val}));
            }
        }
    }

    out.insert("messages".into(), Value::Array(out_messages));

    // tools
    if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
        let out_tools: Vec<Value> = tools.iter().map(convert_tool_def).collect();
        out.insert("tools".into(), Value::Array(out_tools));
    }

    // tool_choice
    if let Some(tc) = obj.get("tool_choice") {
        if let Some(converted) = convert_tool_choice(tc) {
            out.insert("tool_choice".into(), converted);
        }
    }

    let bytes = serde_json::to_vec(&Value::Object(out))?;
    Ok(Bytes::from(bytes))
}

/// Join an Anthropic `system` field (string or array of content blocks) into a
/// single text string for the OpenAI system message.
fn system_to_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for b in blocks {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    parts.push(t.to_string());
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

/// Convert an Anthropic assistant message's content into an OpenAI assistant
/// message. Text blocks become `content` (string or array); `tool_use` blocks
/// become `tool_calls`; `thinking` blocks are dropped.
fn convert_assistant_message(role: &str, content: Option<&Value>) -> Result<Value> {
    let Some(content) = content else {
        return Ok(json!({"role": role, "content": ""}));
    };

    // String content → passthrough.
    if let Some(s) = content.as_str() {
        return Ok(json!({"role": role, "content": s}));
    }

    let blocks = content
        .as_array()
        .ok_or_else(|| CoreError::Proxy("assistant content must be string or array".into()))?;

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for b in blocks {
        let btype = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match btype {
            "text" => {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(t.to_string());
                }
            }
            "tool_use" => {
                let id = b
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = b
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = b
                    .get("input")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }));
            }
            "thinking" => {
                // drop
            }
            _ => {
                // unknown block: drop
            }
        }
    }

    let mut msg = Map::new();
    msg.insert("role".into(), Value::String(role.to_string()));

    if tool_calls.is_empty() {
        // text-only: use string content when single part, else array of parts
        if text_parts.len() == 1 {
            msg.insert(
                "content".into(),
                Value::String(text_parts.into_iter().next().unwrap()),
            );
        } else if text_parts.is_empty() {
            msg.insert("content".into(), Value::String(String::new()));
        } else {
            let parts: Vec<Value> = text_parts
                .into_iter()
                .map(|t| json!({"type": "text", "text": t}))
                .collect();
            msg.insert("content".into(), Value::Array(parts));
        }
    } else {
        // With tool_calls, OpenAI expects content to be null or a string.
        if text_parts.is_empty() {
            msg.insert("content".into(), Value::Null);
        } else {
            msg.insert("content".into(), Value::String(text_parts.join("\n")));
        }
        msg.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    Ok(Value::Object(msg))
}

/// Convert an Anthropic user message's content. A user message may contain
/// text/image blocks (kept as a user message with array content) and
/// `tool_result` blocks (each split into its own `{role:"tool", ...}` message).
fn convert_user_message(content: Option<&Value>, out_messages: &mut Vec<Value>) -> Result<()> {
    let Some(content) = content else {
        out_messages.push(json!({"role": "user", "content": ""}));
        return Ok(());
    };

    // String content → user message with string content.
    if let Some(s) = content.as_str() {
        out_messages.push(json!({"role": "user", "content": s}));
        return Ok(());
    }

    let blocks = content
        .as_array()
        .ok_or_else(|| CoreError::Proxy("user content must be string or array".into()))?;

    let mut parts: Vec<Value> = Vec::new();
    let mut had_tool_result = false;

    for b in blocks {
        let btype = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match btype {
            "text" => {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    parts.push(json!({"type": "text", "text": t}));
                }
            }
            "image" => {
                // Anthropic image block: {source: {type:"base64", media_type, data}}
                let url = anthropic_image_to_data_uri(b);
                parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            "tool_result" => {
                had_tool_result = true;
                let tool_call_id = b
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content_text = tool_result_content_to_text(b.get("content"));
                out_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content_text,
                }));
            }
            "thinking" => {
                // drop
            }
            _ => {
                // unknown: drop
            }
        }
    }

    // Emit a user message for any non-tool_result parts collected.
    if !parts.is_empty() {
        // If only one text part and no tool_result, simplify to string content.
        if parts.len() == 1 && !had_tool_result {
            if let Some(t) = parts[0]
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
            {
                out_messages.push(json!({"role": "user", "content": t}));
                return Ok(());
            }
        }
        out_messages.push(json!({"role": "user", "content": Value::Array(parts)}));
    } else if !had_tool_result {
        // No parts and no tool_result: emit empty user message to preserve message.
        out_messages.push(json!({"role": "user", "content": ""}));
    }

    Ok(())
}

/// Build a data URI from an Anthropic image block's `source` field.
/// Supports `type:"base64"` sources (most common); falls back to empty string.
fn anthropic_image_to_data_uri(block: &Value) -> String {
    let source = match block.get("source") {
        Some(s) => s,
        None => return String::new(),
    };
    let stype = source.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if stype == "base64" {
        let media_type = source
            .get("media_type")
            .and_then(|v| v.as_str())
            .unwrap_or("image/png");
        let data = source.get("data").and_then(|v| v.as_str()).unwrap_or("");
        format!("data:{media_type};base64,{data}")
    } else if stype == "url" {
        source
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    }
}

/// Flatten an Anthropic `tool_result.content` field (string or array of
/// content blocks) into a single text string for the OpenAI tool message.
fn tool_result_content_to_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts: Vec<String> = Vec::new();
        for b in arr {
            if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                parts.push(t.to_string());
            }
        }
        return parts.join("\n");
    }
    String::new()
}

/// Convert an Anthropic tool definition to the OpenAI function-tool shape.
fn convert_tool_def(tool: &Value) -> Value {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = tool
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parameters = tool
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

/// Convert an Anthropic `tool_choice` to the OpenAI `tool_choice` value.
/// Returns `None` to omit the field (e.g. unsupported shape).
fn convert_tool_choice(tc: &Value) -> Option<Value> {
    if let Some(s) = tc.as_str() {
        // Some clients send plain strings; pass through.
        return Some(Value::String(s.to_string()));
    }
    let ttype = tc.get("type").and_then(|v| v.as_str())?;
    match ttype {
        "auto" => Some(Value::String("auto".into())),
        "any" => Some(Value::String("required".into())),
        "tool" => {
            let name = tc.get("name").and_then(|v| v.as_str()).unwrap_or("");
            Some(json!({"type": "function", "function": {"name": name}}))
        }
        _ => None,
    }
}

// ===================== Response side (OpenAI → Anthropic) =====================

/// Convert an OpenAI non-streaming chat completion response body into an
/// Anthropic `/v1/messages` non-streaming response body.
///
/// Behavior (see spec §3 response side, non-streaming):
/// - `choices[0].message.content` → `content[0]` text block (when non-null)
/// - `choices[0].message.tool_calls[]` → `content[]` tool_use blocks
///   (input = arguments JSON parsed; on parse failure use `{}`)
/// - `finish_reason`: stop→`end_turn`, length→`max_tokens`, tool_calls→`tool_use`
/// - `usage`: prompt_tokens→input_tokens, completion_tokens→output_tokens
/// - top-level `id` passthrough; `model` set to `req_model`; `role:"assistant"`,
///   `type:"message"`
pub fn response_openai_to_anthropic(body: &[u8], req_model: &str) -> Result<Bytes> {
    let input: Value = serde_json::from_slice(body)?;
    let obj = input
        .as_object()
        .ok_or_else(|| CoreError::Proxy("openai response body is not a JSON object".into()))?;

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let choice = obj
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| CoreError::Proxy("openai response missing choices[0]".into()))?;

    let message = choice
        .get("message")
        .ok_or_else(|| CoreError::Proxy("openai response missing choices[0].message".into()))?;

    let finish_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop");
    let stop_reason = match finish_reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        other => other, // passthrough unknown reasons verbatim
    };

    // Build content blocks.
    let mut content: Vec<Value> = Vec::new();

    // Text content (skip if null/empty).
    if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }

    // tool_calls → tool_use blocks.
    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let function = tc.get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments_str = function
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            // Parse arguments JSON; fall back to {} on failure.
            let input: Value = serde_json::from_str(arguments_str).unwrap_or(json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }

    // usage
    let usage = obj.get("usage").cloned().unwrap_or(Value::Null);
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let out = json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": req_model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
        }
    });

    let bytes = serde_json::to_vec(&out)?;
    Ok(Bytes::from(bytes))
}

/// Streaming state for the OpenAI→Anthropic SSE conversion state machine.
#[derive(Debug)]
struct StreamState {
    phase: Phase,
    /// Next Anthropic content-block index. Text and tool_use blocks share one
    /// sequential counter — the Anthropic protocol requires unique, increasing
    /// indexes across the whole message (the OpenAI tool-call index is only a
    /// matching key and collides with the text block's index 0).
    next_block: usize,
}

#[derive(Debug)]
enum Phase {
    /// No `message_start` emitted yet.
    Initial,
    /// A text content block is currently open at Anthropic index `block`.
    InTextBlock { block: usize },
    /// A tool_use content block is currently open; `index` is the OpenAI
    /// tool-call index (matching key for argument fragments), `block` the
    /// Anthropic content-block index, `buffer` holds the aggregated
    /// arguments JSON string fragments.
    InToolBlock {
        index: usize,
        block: usize,
        buffer: String,
    },
    /// Terminal: `message_stop` emitted.
    Done,
}

/// Build an Anthropic SSE event frame: `event: <type>\ndata: <json>\n\n`.
fn sse_event(event_type: &str, data: &Value) -> Bytes {
    let data_str = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    let frame = format!("event: {event_type}\ndata: {data_str}\n\n");
    Bytes::from(frame)
}

/// A spec-compliant `content_block_stop` frame (requires `type` + `index`).
fn block_stop_event(block: usize) -> Bytes {
    sse_event(
        "content_block_stop",
        &json!({"type": "content_block_stop", "index": block}),
    )
}

/// Convert a stream of OpenAI chat completion SSE chunks (each `Bytes` is one
/// `data: <json>\n\n` frame) into a stream of Anthropic messages-API SSE event
/// frames.
///
/// Implements the state machine described in spec §3 response side, streaming:
/// - first chunk with `delta.role` → `message_start` + open text block
/// - `delta.content` → `content_block_delta(text_delta)`
/// - `delta.tool_calls[i]` → close text block if open; open tool_use block;
///   aggregate arguments by index; on completion emit `input_json_delta` then
///   `content_block_stop`
/// - `finish_reason` → close any open block; `message_delta(stop_reason, usage)`;
///   `message_stop`
///
/// An upstream error (mid-stream transport failure) is surfaced as an
/// Anthropic `error` event instead of a graceful `message_stop`, so strict
/// clients fail the turn instead of accepting a truncated message.
///
/// Uses `futures::stream::unfold` for true streaming semantics. The upstream
/// stream is carried in the unfold state so it persists across poll cycles.
pub fn stream_openai_to_anthropic<S>(upstream: S, req_model: String) -> impl Stream<Item = Bytes>
where
    S: Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    /// unfold state: (upstream stream, conversion state, pending output queue).
    /// The pending queue holds frames already produced but not yet emitted
    /// (so a single upstream chunk can yield multiple Anthropic frames across
    /// multiple unfold steps without re-polling upstream). The upstream stream
    /// is boxed+pin so it is `Unpin` and can be polled inside the async block.
    struct UnfoldState {
        upstream: std::pin::Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>,
        conv: StreamState,
        pending: std::collections::VecDeque<Bytes>,
        finished: bool,
    }

    let init = UnfoldState {
        upstream: Box::pin(upstream),
        conv: StreamState {
            phase: Phase::Initial,
            next_block: 0,
        },
        pending: std::collections::VecDeque::new(),
        finished: false,
    };

    futures::stream::unfold(init, move |mut st| {
        let req_model = req_model.clone();
        async move {
            loop {
                // If we have pending frames, emit the front one.
                if let Some(frame) = st.pending.pop_front() {
                    return Some((frame, st));
                }
                if st.finished {
                    return None;
                }
                // Need more input from upstream.
                use futures::StreamExt;
                match st.upstream.as_mut().next().await {
                    None => {
                        // Upstream ended: close out gracefully.
                        match &st.conv.phase {
                            Phase::Done => {
                                st.finished = true;
                                continue;
                            }
                            Phase::InTextBlock { block } | Phase::InToolBlock { block, .. } => {
                                st.pending.push_back(block_stop_event(*block));
                            }
                            _ => {}
                        }
                        st.pending.push_back(sse_event(
                            "message_delta",
                            &json!({
                                "delta": {"stop_reason": Value::Null, "stop_sequence": Value::Null},
                                "usage": {"output_tokens": Value::Null}
                            }),
                        ));
                        st.pending.push_back(sse_event("message_stop", &json!({})));
                        st.conv.phase = Phase::Done;
                        st.finished = true;
                        continue;
                    }
                    Some(Err(e)) => {
                        // Upstream failed mid-stream: surface an error event so
                        // clients fail the turn instead of accepting a
                        // truncated message.
                        st.pending.push_back(sse_event(
                            "error",
                            &json!({
                                "type": "error",
                                "error": {"type": "api_error", "message": format!("upstream stream interrupted: {e}")}
                            }),
                        ));
                        st.conv.phase = Phase::Done;
                        st.finished = true;
                        continue;
                    }
                    Some(Ok(chunk)) => {
                        let text = String::from_utf8_lossy(&chunk);
                        let parsed = parse_openai_sse_frames(&text);
                        let (new_conv, out_frames) = process_frames(st.conv, &parsed, &req_model);
                        st.conv = new_conv;
                        for f in out_frames {
                            st.pending.push_back(f);
                        }
                        // If we produced nothing (e.g. empty chunk), loop to
                        // pull more upstream.
                        continue;
                    }
                }
            }
        }
    })
}

/// Parse one or more `data: <json>\n\n` frames from a chunk into JSON values.
/// Ignores `data: [DONE]` and malformed lines.
fn parse_openai_sse_frames(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            if rest == "[DONE]" || rest.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(rest) {
                out.push(v);
            }
        }
    }
    out
}

/// Process a batch of parsed OpenAI chunk JSON values against the current
/// state, returning the new state and any Anthropic SSE frames to emit.
fn process_frames(
    mut state: StreamState,
    frames: &[Value],
    req_model: &str,
) -> (StreamState, Vec<Bytes>) {
    let mut out: Vec<Bytes> = Vec::new();
    let mut message_id: Option<String> = None;
    let mut output_tokens: Option<u64> = None;

    for frame in frames {
        // Ignore anything after the terminal event (some gateways send
        // trailing frames past finish_reason).
        if matches!(state.phase, Phase::Done) {
            break;
        }
        // Capture id at top level for message_start.
        if message_id.is_none() {
            if let Some(id) = frame.get("id").and_then(|v| v.as_str()) {
                message_id = Some(id.to_string());
            }
        }
        // Capture usage output_tokens.
        if let Some(usage) = frame.get("usage") {
            if let Some(ot) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                output_tokens = Some(ot);
            }
        }

        let choice = match frame
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
        {
            Some(c) => c,
            None => continue,
        };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

        // First chunk → message_start. Text and tool blocks are opened lazily
        // (on first non-empty content / first tool_call), so an empty text
        // block is never emitted ahead of a tool_use block.
        if matches!(state.phase, Phase::Initial) {
            let id = message_id.clone().unwrap_or_default();
            out.push(sse_event(
                "message_start",
                &json!({
                    "type": "message_start",
                    "message": {
                        "id": id,
                        "type": "message",
                        "role": "assistant",
                        "model": req_model,
                        "content": [],
                        "stop_reason": Value::Null,
                        "stop_sequence": Value::Null,
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    }
                }),
            ));
        }

        // delta.content text → text_delta.
        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                if matches!(state.phase, Phase::Initial) {
                    // Open a text block now.
                    let block = state.next_block;
                    state.next_block += 1;
                    state.phase = Phase::InTextBlock { block };
                    out.push(sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": block,
                            "content_block": {"type": "text", "text": ""}
                        }),
                    ));
                }
                if let Phase::InTextBlock { block } = &state.phase {
                    out.push(sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": block,
                            "delta": {"type": "text_delta", "text": text}
                        }),
                    ));
                }
            }
        }

        // delta.tool_calls → tool_use blocks.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let function = tc.get("function").cloned().unwrap_or(Value::Null);
                let name = function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments_frag = function
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // If a text block is open, close it.
                if let Phase::InTextBlock { block } = &state.phase {
                    out.push(block_stop_event(*block));
                }
                // If a different tool block is open, finalize it.
                if let Phase::InToolBlock {
                    index: cur,
                    block: cur_block,
                    buffer,
                } = &state.phase
                {
                    if *cur != index {
                        // Finalize previous tool block: emit aggregated JSON.
                        out.push(sse_event(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": cur_block,
                                "delta": {"type": "input_json_delta", "partial_json": buffer}
                            }),
                        ));
                        out.push(block_stop_event(*cur_block));
                    }
                }

                // If this is a new tool block (not continuing), open it.
                let is_new_block = match &state.phase {
                    Phase::InToolBlock {
                        index: cur, buffer, ..
                    } => *cur != index || (!id.is_empty() && buffer.is_empty()),
                    _ => true,
                };
                if is_new_block {
                    let block = state.next_block;
                    state.next_block += 1;
                    out.push(sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": block,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": {}
                            }
                        }),
                    ));
                    state.phase = Phase::InToolBlock {
                        index,
                        block,
                        buffer: String::new(),
                    };
                }

                // Append arguments fragment to the buffer.
                if let Phase::InToolBlock { buffer, .. } = &mut state.phase {
                    buffer.push_str(arguments_frag);
                }
            }
        }

        // finish_reason → close any open block + message_delta + message_stop.
        if let Some(fr) = finish_reason {
            let stop_reason = match fr {
                "stop" => "end_turn",
                "length" => "max_tokens",
                "tool_calls" => "tool_use",
                other => other,
            };
            // Finalize the currently-open block.
            match &state.phase {
                Phase::InTextBlock { block } => {
                    out.push(block_stop_event(*block));
                }
                Phase::InToolBlock { block, buffer, .. } => {
                    // Emit the aggregated JSON as a single input_json_delta.
                    out.push(sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": block,
                            "delta": {"type": "input_json_delta", "partial_json": buffer}
                        }),
                    ));
                    out.push(block_stop_event(*block));
                }
                _ => {}
            }
            let ot = output_tokens.map(Value::from).unwrap_or(Value::Null);
            out.push(sse_event(
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
                    "usage": {"output_tokens": ot}
                }),
            ));
            out.push(sse_event("message_stop", &json!({"type": "message_stop"})));
            state.phase = Phase::Done;
        }
    }

    (state, out)
}

// ── OpenAI Responses ↔ Chat Completions conversion ─────────────────────────
//
// Codex speaks the Responses API (`POST /v1/responses`) exclusively (its
// `wire_api = "chat"` mode was removed in 0.155). These functions let a
// Responses client talk to a chat-completions-only upstream through xfade:
//   - `request_responses_to_chat`: Responses request → Chat request.
//   - `response_chat_to_responses` / `stream_chat_to_responses`: Chat
//     response (JSON or SSE) → Responses response (JSON or SSE events).
//
// The event contract matches what Codex consumes
// (codex-api/src/sse/responses.rs): `response.created`,
// `response.output_text.delta`, `response.output_item.done` (message /
// function_call items) and `response.completed` (with `response.usage`).

/// Convert an OpenAI `/v1/responses` request body into a
/// `/v1/chat/completions` request body.
pub fn request_responses_to_chat(body: &[u8], model_override: Option<&str>) -> Result<Bytes> {
    let input: Value = serde_json::from_slice(body)?;
    let obj = input
        .as_object()
        .ok_or_else(|| CoreError::Proxy("responses request body is not a JSON object".into()))?;
    let mut out = Map::new();

    let model = model_override
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            obj.get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
    if let Some(m) = model {
        out.insert("model".into(), Value::String(m));
    }

    for key in ["stream", "temperature", "top_p", "parallel_tool_calls"] {
        if let Some(v) = obj.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if let Some(v) = obj.get("max_output_tokens") {
        out.insert("max_tokens".into(), v.clone());
    }
    if let Some(v) = obj.get("tool_choice") {
        if v.is_string() || v.is_object() {
            out.insert("tool_choice".into(), v.clone());
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = obj.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            messages.push(json!({"role": "system", "content": instructions}));
        }
    }

    match obj.get("input").cloned().unwrap_or(Value::Null) {
        Value::String(s) => messages.push(json!({"role": "user", "content": s})),
        Value::Array(items) => {
            for item in &items {
                responses_item_to_chat(item, &mut messages);
            }
        }
        Value::Null => {}
        _ => {
            return Err(CoreError::Proxy(
                "responses request 'input' must be a string or array".into(),
            ))
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
        let chat_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?;
                let mut f = Map::new();
                f.insert("name".into(), json!(name));
                if let Some(d) = t.get("description") {
                    f.insert("description".into(), d.clone());
                }
                if let Some(p) = t.get("parameters") {
                    f.insert("parameters".into(), p.clone());
                }
                Some(json!({"type": "function", "function": Value::Object(f)}))
            })
            .collect();
        if !chat_tools.is_empty() {
            out.insert("tools".into(), Value::Array(chat_tools));
        }
    }

    Ok(Bytes::from(serde_json::to_vec(&Value::Object(out))?))
}

/// Map one Responses `input` item into chat messages.
/// `reasoning` items (and unknown types) are dropped — they have no
/// chat-completions equivalent.
fn responses_item_to_chat(item: &Value, messages: &mut Vec<Value>) {
    let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match ty {
        "message" => {
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let role = match role {
                "developer" | "system" => "system",
                r => r,
            };
            let text: String = item
                .get("content")
                .and_then(|c| c.as_array())
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            messages.push(json!({"role": role, "content": text}));
        }
        "function_call" => {
            let call = json!({
                "id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default(),
                "type": "function",
                "function": {
                    "name": item.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "arguments": item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
                }
            });
            // Consecutive function_call items merge into one assistant message.
            match messages.last_mut() {
                Some(Value::Object(m))
                    if m.get("role").and_then(|r| r.as_str()) == Some("assistant")
                        && m.get("tool_calls").is_some() =>
                {
                    if let Some(a) = m.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                        a.push(call);
                    }
                }
                _ => messages
                    .push(json!({"role": "assistant", "content": null, "tool_calls": [call]})),
            }
        }
        "function_call_output" => {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default(),
                "content": function_call_output_text(item.get("output")),
            }));
        }
        _ => {}
    }
}

/// Responses `function_call_output.output` is either a plain string or an
/// array of `{type:"output_text"|"input_text", text}` content items.
fn function_call_output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        Some(Value::Object(o)) => o
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Build a Responses `response.completed`-style usage block from a chat
/// usage block (prompt/completion → input/output).
fn responses_usage(usage: &Value) -> Value {
    json!({
        "input_tokens": usage.get("prompt_tokens").and_then(|t| t.as_i64()).unwrap_or(0),
        "output_tokens": usage.get("completion_tokens").and_then(|t| t.as_i64()).unwrap_or(0),
        "total_tokens": usage.get("total_tokens").and_then(|t| t.as_i64()).unwrap_or(0),
    })
}

/// Convert a Chat Completions JSON response body into a Responses API
/// response object.
pub fn response_chat_to_responses(body: &[u8], req_model: &str) -> Result<Bytes> {
    let v: Value = serde_json::from_slice(body)?;
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("resp-xfade")
        .to_string();
    let output = chat_message_to_response_items(&v);
    let resp = json!({
        "id": id,
        "object": "response",
        "status": "completed",
        "model": req_model,
        "output": output,
        "usage": responses_usage(v.get("usage").unwrap_or(&Value::Null)),
    });
    Ok(Bytes::from(serde_json::to_vec(&resp)?))
}

/// Extract the Responses output items (message / function_call) from a chat
/// completion response object (`choices[0].message`).
fn chat_message_to_response_items(v: &Value) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    let Some(choice) = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    else {
        return output;
    };
    let msg = choice.get("message").cloned().unwrap_or(Value::Null);
    if let Some(t) = msg.get("content").and_then(|c| c.as_str()) {
        if !t.is_empty() {
            output.push(json!({
                "type": "message",
                "id": "msg-xfade-0",
                "role": "assistant",
                "content": [{"type": "output_text", "text": t}],
            }));
        }
    }
    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for (i, tc) in tcs.iter().enumerate() {
            output.push(json!({
                "type": "function_call",
                "id": format!("fc-xfade-{i}"),
                "call_id": tc.get("id").and_then(|x| x.as_str()).unwrap_or_default(),
                "name": tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or_default(),
                "arguments": tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("{}"),
            }));
        }
    }
    output
}

/// Streaming state for the Chat → Responses SSE conversion state machine.
#[derive(Debug, Default)]
struct ResponsesStreamState {
    created: bool,
    /// Whether `response.output_item.added` for the assistant message was
    /// emitted (Codex requires it before consuming `output_text.delta`).
    msg_added: bool,
    resp_id: String,
    model: String,
    text: String,
    /// Aggregated tool calls keyed by the OpenAI tool-call index:
    /// (call_id, name, arguments buffer).
    tools: Vec<(usize, String, String, String)>,
    usage: Option<Value>,
    finished: bool,
}

/// Convert a stream of OpenAI chat completion SSE chunks into a stream of
/// Responses API SSE events, mirroring `stream_openai_to_anthropic`.
///
/// An upstream error (mid-stream transport failure) is surfaced as a
/// `response.failed` event so Responses clients (Codex) treat the turn as
/// failed and retry, instead of accepting a truncated message.
pub fn stream_chat_to_responses<S>(upstream: S, req_model: String) -> impl Stream<Item = Bytes>
where
    S: Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    struct UnfoldState {
        upstream: std::pin::Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>,
        st: ResponsesStreamState,
        pending: std::collections::VecDeque<Bytes>,
    }

    let init = UnfoldState {
        upstream: Box::pin(upstream),
        st: ResponsesStreamState {
            model: req_model,
            ..Default::default()
        },
        pending: std::collections::VecDeque::new(),
    };

    futures::stream::unfold(init, move |mut st| async move {
        loop {
            if let Some(frame) = st.pending.pop_front() {
                return Some((frame, st));
            }
            use futures::StreamExt;
            match st.upstream.as_mut().next().await {
                None => {
                    if st.st.finished {
                        return None;
                    }
                    for f in chat_to_responses_finish(&mut st.st) {
                        st.pending.push_back(f);
                    }
                    st.st.finished = true;
                    continue;
                }
                Some(Err(e)) => {
                    if !st.st.created {
                        st.st.created = true;
                        st.pending.push_back(sse_event(
                            "response.created",
                            &json!({"type": "response.created", "response": {"id": st.st.resp_id}}),
                        ));
                    }
                    st.pending.push_back(sse_event(
                            "response.failed",
                            &json!({
                                "type": "response.failed",
                                "response": {
                                    "id": st.st.resp_id,
                                    "error": {"code": "upstream_error", "message": format!("upstream stream interrupted: {e}")}
                                }
                            }),
                        ));
                    st.st.finished = true;
                    continue;
                }
                Some(Ok(chunk)) => {
                    let text = String::from_utf8_lossy(&chunk);
                    let frames = parse_openai_sse_frames(&text);
                    let out = process_chat_chunk_responses(&mut st.st, &frames);
                    for f in out {
                        st.pending.push_back(f);
                    }
                    continue;
                }
            }
        }
    })
}

/// Process one batch of parsed chat chunk JSON values, returning SSE frames.
fn process_chat_chunk_responses(st: &mut ResponsesStreamState, frames: &[Value]) -> Vec<Bytes> {
    let mut out = Vec::new();
    for frame in frames {
        // Ignore anything after the terminal chunk (some gateways send
        // trailing frames past finish_reason).
        if st.finished {
            break;
        }
        if st.resp_id.is_empty() {
            if let Some(id) = frame.get("id").and_then(|v| v.as_str()) {
                st.resp_id = id.to_string();
            }
        }
        if let Some(usage) = frame.get("usage") {
            if usage.is_object() && !usage.as_object().unwrap().is_empty() {
                st.usage = Some(usage.clone());
            }
        }
        let Some(choice) = frame
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
        else {
            continue;
        };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

        if !st.created {
            st.created = true;
            out.push(sse_event(
                "response.created",
                &json!({"type": "response.created", "response": {"id": st.resp_id}}),
            ));
        }

        // Text deltas.
        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                if !st.msg_added {
                    st.msg_added = true;
                    out.push(sse_event(
                        "response.output_item.added",
                        &json!({
                            "type": "response.output_item.added",
                            "output_index": 0,
                            "item": {"type": "message", "id": "msg-xfade-0", "role": "assistant", "content": [{"type": "output_text", "text": ""}]}
                        }),
                    ));
                }
                st.text.push_str(text);
                out.push(sse_event(
                    "response.output_text.delta",
                    &json!({"type": "response.output_text.delta", "item_id": "msg-xfade-0", "output_index": 0, "content_index": 0, "delta": text}),
                ));
            }
        }

        // Tool call argument fragments (keyed by the OpenAI tool index).
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let entry = match st.tools.iter_mut().find(|(i, _, _, _)| *i == index) {
                    Some(e) => e,
                    None => {
                        st.tools
                            .push((index, String::new(), String::new(), String::new()));
                        st.tools.last_mut().unwrap()
                    }
                };
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    if !id.is_empty() {
                        entry.1 = id.to_string();
                    }
                }
                if let Some(name) = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                {
                    if !name.is_empty() {
                        entry.2 = name.to_string();
                    }
                }
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    entry.3.push_str(args);
                }
            }
        }

        // Terminal chunk.
        if choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .is_some()
        {
            out.extend(chat_to_responses_finish(st));
            st.finished = true;
        }
    }
    out
}

/// Emit the terminal Responses events (message/function_call items +
/// `response.completed`). Returns the frames; safe to call once.
fn chat_to_responses_finish(st: &mut ResponsesStreamState) -> Vec<Bytes> {
    let mut out = Vec::new();
    if !st.created {
        st.created = true;
        out.push(sse_event(
            "response.created",
            &json!({"type": "response.created", "response": {"id": st.resp_id}}),
        ));
    }
    if st.finished {
        return out;
    }

    let mut items: Vec<Value> = Vec::new();
    if !st.text.is_empty() {
        items.push(json!({
            "type": "message",
            "id": "msg-xfade-0",
            "role": "assistant",
            "content": [{"type": "output_text", "text": st.text}],
        }));
    }
    for (_, call_id, name, args) in &st.tools {
        items.push(json!({
            "type": "function_call",
            "id": format!("fc-xfade-{}", items.len()),
            "call_id": call_id,
            "name": name,
            "arguments": if args.is_empty() { "{}".to_string() } else { args.clone() },
        }));
    }
    if items.is_empty() {
        // Keep at least one assistant message item so the turn is not empty.
        items.push(json!({
            "type": "message",
            "id": "msg-xfade-0",
            "role": "assistant",
            "content": [{"type": "output_text", "text": ""}],
        }));
    }
    for (output_index, item) in items.iter().enumerate() {
        out.push(sse_event(
            "response.output_item.done",
            &json!({"type": "response.output_item.done", "output_index": output_index, "item": item}),
        ));
    }
    let usage = st
        .usage
        .as_ref()
        .map(responses_usage)
        .unwrap_or_else(|| json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}));
    out.push(sse_event(
        "response.completed",
        &json!({
            "type": "response.completed",
            "response": {
                "id": st.resp_id,
                "model": st.model,
                "status": "completed",
                "output": items,
                "usage": usage,
            }
        }),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_text_request() {
        let inp = br#"{"model":"claude-sonnet-4-6","max_tokens":100,"messages":[{"role":"user","content":"hi"}]}"#;
        let out = request_anthropic_to_openai(inp, Some("gpt-5.6-luna")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "gpt-5.6-luna");
        assert_eq!(v["max_tokens"], 100);
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "hi");
    }

    #[test]
    fn system_string_to_message() {
        let inp = br#"{"model":"m","max_tokens":1,"system":"you are nice","messages":[{"role":"user","content":"x"}]}"#;
        let out = request_anthropic_to_openai(inp, None).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"], "you are nice");
        assert_eq!(v["messages"][1]["content"], "x");
    }

    #[test]
    fn tool_use_and_result_roundtrip() {
        let inp = br#"{"model":"m","max_tokens":1,"messages":[
            {"role":"user","content":"list files"},
            {"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"ls","input":{"path":"/"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"file1\nfile2"}]}
        ]}"#;
        let out = request_anthropic_to_openai(inp, None).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // assistant message should have tool_calls
        let asst = &v["messages"][1];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["tool_calls"][0]["id"], "t1");
        assert_eq!(asst["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(
            asst["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"/"}"#
        );
        // tool_result → role=tool message
        let tool_msg = &v["messages"][2];
        assert_eq!(tool_msg["role"], "tool");
        assert_eq!(tool_msg["tool_call_id"], "t1");
        assert_eq!(tool_msg["content"], "file1\nfile2");
    }

    #[test]
    fn tools_definition_conversion() {
        let inp = br#"{"model":"m","max_tokens":1,"tools":[{"name":"ls","description":"list","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}],"messages":[{"role":"user","content":"x"}]}"#;
        let out = request_anthropic_to_openai(inp, None).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["function"]["name"], "ls");
        assert_eq!(v["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn tool_choice_mapping() {
        let inp = br#"{"model":"m","max_tokens":1,"tool_choice":{"type":"any"},"messages":[{"role":"user","content":"x"}]}"#;
        let out = request_anthropic_to_openai(inp, None).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tool_choice"], "required");
    }

    // ----- C3: response-side conversion tests -----

    #[test]
    fn non_stream_response_with_tool_use() {
        let inp = br#"{"id":"r1","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"t1","type":"function","function":{"name":"ls","arguments":"{\"path\":\"/\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":10}}"#;
        let out = response_openai_to_anthropic(inp, "claude-sonnet-4-6").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["model"], "claude-sonnet-4-6");
        assert_eq!(v["content"][0]["type"], "tool_use");
        assert_eq!(v["content"][0]["id"], "t1");
        assert_eq!(v["content"][0]["name"], "ls");
        assert_eq!(v["content"][0]["input"]["path"], "/");
        assert_eq!(v["stop_reason"], "tool_use");
        assert_eq!(v["usage"]["input_tokens"], 5);
        assert_eq!(v["usage"]["output_tokens"], 10);
    }

    #[test]
    fn non_stream_text_response() {
        let inp = br#"{"id":"r1","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        let out = response_openai_to_anthropic(inp, "m").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "hello");
        assert_eq!(v["stop_reason"], "end_turn");
    }

    #[tokio::test]
    async fn stream_text_passthrough() {
        // OpenAI chunks: role+content "h", content "i", finish stop
        let chunks = vec![
            ok_chunk(br#"{"choices":[{"delta":{"role":"assistant","content":"h"}}]}"#),
            ok_chunk(br#"{"choices":[{"delta":{"content":"i"}}]}"#),
            ok_chunk(br#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#),
        ];
        let stream = futures::stream::iter(chunks.into_iter().map(Ok));
        let out_bytes = collect_stream(stream_openai_to_anthropic(stream, "m".into())).await;
        let s = String::from_utf8(out_bytes).unwrap();
        assert!(s.contains("message_start"));
        assert!(s.contains("content_block_start"));
        assert!(s.contains(r#""text":"h","type":"text_delta""#));
        assert!(s.contains(r#""text":"i","type":"text_delta""#));
        assert!(s.contains("content_block_stop"));
        assert!(s.contains("message_delta"));
        assert!(s.contains("message_stop"));
        assert!(s.contains(r#""output_tokens":2"#));
    }

    #[tokio::test]
    async fn stream_tool_use_aggregation() {
        // tool_calls arguments fragments: "{\"pa" + `th":"/"}`, joined into `{"path":"/"}`
        let chunks = vec![
            ok_chunk(br#"{"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"ls","arguments":"{\"pa"}}]}}]}"#),
            ok_chunk(br#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"/\"}"}}]}}]}"#),
            ok_chunk(br#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":3}}"#),
        ];
        let stream = futures::stream::iter(chunks.into_iter().map(Ok));
        let out_bytes = collect_stream(stream_openai_to_anthropic(stream, "m".into())).await;
        let s = String::from_utf8(out_bytes).unwrap();
        // should have a content_block_start tool_use
        assert!(s.contains(r#""type":"tool_use""#));
        assert!(s.contains(r#""id":"t1""#));
        assert!(s.contains(r#""name":"ls""#));
        // aggregated full JSON emitted in one shot
        assert!(s.contains(r#""input_json_delta""#));
        assert!(s.contains(r#""partial_json":"{\"path\":\"/\"}""#));
        assert!(s.contains("content_block_stop"));
        assert!(s.contains(r#""stop_reason":"tool_use""#));
    }

    #[tokio::test]
    async fn stream_text_then_tool_assigns_sequential_indexes_and_spec_stop_frames() {
        // Regression: the OpenAI tool index (0) used to collide with the text
        // block's index 0, and content_block_stop carried an empty payload.
        let chunks = vec![
            ok_chunk(br#"{"choices":[{"delta":{"role":"assistant","content":"hi"}}]}"#),
            ok_chunk(br#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"ls","arguments":"{\"path\":\"/\"}"}}]}}]}"#),
            ok_chunk(br#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ];
        let out_bytes = collect_stream(stream_openai_to_anthropic(
            futures::stream::iter(chunks.into_iter().map(Ok)),
            "m".into(),
        ))
        .await;
        let s = String::from_utf8(out_bytes).unwrap();
        let lines: Vec<&str> = s.lines().collect();

        // text block opens at index 0, tool_use block at index 1 — no collision
        assert!(lines
            .iter()
            .any(|l| l.contains(r#""type":"content_block_start""#)
                && l.contains(r#""type":"text""#)
                && l.contains(r#""index":0"#)));
        assert!(lines
            .iter()
            .any(|l| l.contains(r#""type":"content_block_start""#)
                && l.contains(r#""type":"tool_use""#)
                && l.contains(r#""index":1"#)));

        // every content_block_stop carries type + index (Anthropic spec),
        // one stop for each of the two blocks
        let stops: Vec<&str> = lines
            .iter()
            .filter(|l| l.contains(r#""type":"content_block_stop""#))
            .copied()
            .collect();
        assert_eq!(stops.len(), 2, "expected stops for text + tool blocks");
        assert_eq!(
            stops.iter().filter(|l| l.contains(r#""index":0"#)).count(),
            1
        );
        assert_eq!(
            stops.iter().filter(|l| l.contains(r#""index":1"#)).count(),
            1
        );

        // the aggregated tool arguments stay attached to the tool block
        assert!(lines
            .iter()
            .any(|l| l.contains(r#""input_json_delta""#) && l.contains(r#""index":1"#)));
    }

    #[test]
    fn responses_request_basic() {
        let inp = br#"{"model":"gpt-x","instructions":"be brief","stream":true,
"max_output_tokens":100,
"input":[
  {"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]},
  {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
  {"type":"function_call","name":"ls","arguments":"{\"path\":\"/\"}","call_id":"c1"},
  {"type":"function_call_output","call_id":"c1","output":"file1"},
  {"type":"reasoning","summary":[]}
],
"tools":[{"type":"function","name":"ls","description":"list","parameters":{"type":"object"},"strict":true}],
"tool_choice":"auto","parallel_tool_calls":false,
"reasoning":{"effort":"low"},"store":false}"#;
        let out = request_responses_to_chat(inp, Some("glm-5-2")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "glm-5-2");
        assert_eq!(v["max_tokens"], 100);
        assert_eq!(v["tool_choice"], "auto");
        assert_eq!(v["parallel_tool_calls"], false);
        assert!(v.get("reasoning").is_none());
        assert!(v.get("store").is_none());
        // instructions → system; developer → system
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"], "be brief");
        assert_eq!(v["messages"][1]["role"], "system");
        assert_eq!(v["messages"][1]["content"], "rules");
        assert_eq!(v["messages"][2]["content"], "hi");
        // function_call → assistant tool_calls
        assert_eq!(v["messages"][3]["tool_calls"][0]["id"], "c1");
        assert_eq!(v["messages"][3]["tool_calls"][0]["function"]["name"], "ls");
        // function_call_output → tool message
        assert_eq!(v["messages"][4]["role"], "tool");
        assert_eq!(v["messages"][4]["tool_call_id"], "c1");
        assert_eq!(v["messages"][4]["content"], "file1");
        // tools mapped to chat shape
        assert_eq!(v["tools"][0]["function"]["name"], "ls");
        assert!(v["tools"][0].get("strict").is_none());
    }

    #[tokio::test]
    async fn stream_chat_to_responses_text_and_tool() {
        let chunks = vec![
            ok_chunk(br#"{"id":"chatcmpl-1","choices":[{"delta":{"role":"assistant","content":"He"}}]}"#),
            ok_chunk(br#"{"id":"chatcmpl-1","choices":[{"delta":{"content":"llo"}}]}"#),
            ok_chunk(br#"{"id":"chatcmpl-1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"ls","arguments":"{\"pa"}}]}}]}"#),
            ok_chunk(br#"{"id":"chatcmpl-1","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"/\"}"}}]}}]}"#),
            ok_chunk(br#"{"id":"chatcmpl-1","choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}}"#),
        ];
        let out_bytes = collect_stream(stream_chat_to_responses(
            futures::stream::iter(chunks.into_iter().map(Ok)),
            "m".into(),
        ))
        .await;
        let s = String::from_utf8(out_bytes).unwrap();

        // event sequence: created → text deltas → message item → function_call item → completed
        let created = s.find("response.created").unwrap();
        let delta = s.find("response.output_text.delta").unwrap();
        let msg_done = s.find("response.output_item.done").unwrap();
        let completed = s.find("response.completed").unwrap();
        assert!(created < delta && delta < msg_done && msg_done < completed);

        assert!(s.contains(r#""delta":"He""#));
        assert!(s.contains(r#""delta":"llo""#));
        // message item carries the full text
        assert!(
            s.contains(r#""content":[{"type":"output_text","text":"Hello"}]}"#) || {
                // key order may differ under BTreeMap; check both parts on one item line
                s.lines()
                    .any(|l| l.contains(r#""type":"message""#) && l.contains(r#""text":"Hello""#))
            }
        );
        // function_call item carries call_id, name and the full aggregated arguments
        assert!(s.lines().any(|l| l.contains(r#""type":"function_call""#)
            && l.contains(r#""call_id":"c1""#)
            && l.contains(r#""name":"ls""#)
            && l.contains(r#""arguments":"{\"path\":\"/\"}""#)));
        // completed carries the usage block and the real model
        assert!(s.lines().any(|l| l.contains("response.completed")
            && l.contains(r#""input_tokens":3"#)
            && l.contains(r#""output_tokens":5"#)));
        assert!(s
            .lines()
            .any(|l| l.contains("response.completed") && l.contains(r#""model":"m""#)));
    }

    #[tokio::test]
    async fn stream_chat_to_responses_upstream_error_emits_failed() {
        // A mid-stream transport failure must surface as response.failed, not
        // a graceful response.completed (Codex retries the former).
        let chunks: Vec<std::io::Result<Bytes>> = vec![
            Ok(ok_chunk(
                br#"{"id":"chatcmpl-1","choices":[{"delta":{"content":"hi"}}]}"#,
            )),
            Err(std::io::Error::other("connection reset")),
        ];
        let out_bytes = collect_stream(stream_chat_to_responses(
            futures::stream::iter(chunks),
            "m".into(),
        ))
        .await;
        let s = String::from_utf8(out_bytes).unwrap();
        assert!(s.contains("response.failed"), "body: {s}");
        assert!(s.contains("connection reset"), "body: {s}");
        assert!(!s.contains("response.completed"), "body: {s}");
    }

    #[tokio::test]
    async fn stream_chat_to_responses_ignores_frames_after_finish() {
        // Some gateways send trailing frames past finish_reason.
        let chunks = vec![
            ok_chunk(br#"{"id":"c1","choices":[{"delta":{"content":"a"}}]}"#),
            ok_chunk(br#"{"id":"c1","choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            ok_chunk(br#"{"id":"c1","choices":[{"delta":{"content":"LATE"}}]}"#),
        ];
        let out_bytes = collect_stream(stream_chat_to_responses(
            futures::stream::iter(chunks.into_iter().map(Ok)),
            "m".into(),
        ))
        .await;
        let s = String::from_utf8(out_bytes).unwrap();
        let completed = s.find("response.completed").unwrap();
        assert!(!s[completed..].contains("LATE"), "body: {s}");
    }

    #[tokio::test]
    async fn stream_anthropic_upstream_error_emits_error_event() {
        let chunks: Vec<std::io::Result<Bytes>> = vec![
            Ok(ok_chunk(br#"{"choices":[{"delta":{"content":"hi"}}]}"#)),
            Err(std::io::Error::other("connection reset")),
        ];
        let out_bytes = collect_stream(stream_openai_to_anthropic(
            futures::stream::iter(chunks),
            "m".into(),
        ))
        .await;
        let s = String::from_utf8(out_bytes).unwrap();
        assert!(s.contains(r#""type":"error""#), "body: {s}");
        assert!(!s.contains("message_stop"), "body: {s}");
    }

    /// Build a `data: <bytes>\n\n` SSE frame as `Bytes` for testing.
    fn ok_chunk(bytes: &[u8]) -> Bytes {
        let mut frame = b"data: ".to_vec();
        frame.extend_from_slice(bytes);
        frame.extend_from_slice(b"\n\n");
        Bytes::from(frame)
    }

    /// Collect all items from a stream of `Bytes` into a single `Vec<u8>`.
    async fn collect_stream<S>(stream: S) -> Vec<u8>
    where
        S: futures::Stream<Item = Bytes>,
    {
        use futures::StreamExt;
        let mut out = Vec::new();
        let mut s = Box::pin(stream);
        while let Some(chunk) = s.next().await {
            out.extend_from_slice(&chunk);
        }
        out
    }
}
