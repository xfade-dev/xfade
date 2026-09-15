//! Protocol conversion: Anthropic messages → OpenAI chat/completions.
//!
//! This module contains:
//! - Request-side conversion (Anthropic → OpenAI), see `request_anthropic_to_openai`.
//! - Response-side conversion (OpenAI → Anthropic), non-stream + streaming
//!   state machine, see `response_openai_to_anthropic` and
//!   `stream_openai_to_anthropic`. See spec §3 response side.

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
enum StreamState {
    /// No `message_start` emitted yet.
    Initial,
    /// A text content block (index 0) is currently open.
    InTextBlock,
    /// A tool_use content block at `index` is currently open; `buffer` holds
    /// the aggregated arguments JSON string fragments.
    InToolBlock { index: usize, buffer: String },
    /// Terminal: `message_stop` emitted.
    Done,
}

/// Build an Anthropic SSE event frame: `event: <type>\ndata: <json>\n\n`.
fn sse_event(event_type: &str, data: &Value) -> Bytes {
    let data_str = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    let frame = format!("event: {event_type}\ndata: {data_str}\n\n");
    Bytes::from(frame)
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
/// Uses `futures::stream::unfold` for true streaming semantics. The upstream
/// stream is carried in the unfold state so it persists across poll cycles.
pub fn stream_openai_to_anthropic<S>(upstream: S, req_model: String) -> impl Stream<Item = Bytes>
where
    S: Stream<Item = Bytes> + Send + 'static,
{
    /// unfold state: (upstream stream, conversion state, pending output queue).
    /// The pending queue holds frames already produced but not yet emitted
    /// (so a single upstream chunk can yield multiple Anthropic frames across
    /// multiple unfold steps without re-polling upstream). The upstream stream
    /// is boxed+pin so it is `Unpin` and can be polled inside the async block.
    struct UnfoldState {
        upstream: std::pin::Pin<Box<dyn Stream<Item = Bytes> + Send>>,
        conv: StreamState,
        pending: std::collections::VecDeque<Bytes>,
        finished: bool,
    }

    let init = UnfoldState {
        upstream: Box::pin(upstream),
        conv: StreamState::Initial,
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
                        match &st.conv {
                            StreamState::Done => {
                                st.finished = true;
                                continue;
                            }
                            StreamState::InTextBlock | StreamState::InToolBlock { .. } => {
                                st.pending
                                    .push_back(sse_event("content_block_stop", &json!({})));
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
                        st.conv = StreamState::Done;
                        st.finished = true;
                        continue;
                    }
                    Some(chunk) => {
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

        // First chunk with role → message_start + open text block (lazily).
        let has_role = delta.get("role").and_then(|v| v.as_str()).is_some();
        if matches!(state, StreamState::Initial) {
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
            // Decide initial block: if a tool_call is present, open tool block;
            // else open text block (index 0). We handle tool_calls below, so
            // only open text block here if no tool_calls and has content or role.
            let tool_calls_present = delta
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !tool_calls_present {
                out.push(sse_event(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "text", "text": ""}
                    }),
                ));
                state = StreamState::InTextBlock;
            } else {
                // Will be handled in the tool_calls branch below.
                state = StreamState::Initial; // remain, tool branch will transition
            }
            let _ = has_role;
        }

        // delta.content text → text_delta.
        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                if matches!(state, StreamState::Initial) {
                    // Open a text block now.
                    out.push(sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": 0,
                            "content_block": {"type": "text", "text": ""}
                        }),
                    ));
                    state = StreamState::InTextBlock;
                }
                if matches!(state, StreamState::InTextBlock) {
                    out.push(sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": 0,
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
                if matches!(state, StreamState::InTextBlock) {
                    out.push(sse_event("content_block_stop", &json!({})));
                }
                // If a different tool block is open, finalize it.
                if let StreamState::InToolBlock { index: cur, buffer } = &state {
                    if *cur != index {
                        // Finalize previous tool block: emit aggregated JSON.
                        out.push(sse_event(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": cur,
                                "delta": {"type": "input_json_delta", "partial_json": buffer}
                            }),
                        ));
                        out.push(sse_event("content_block_stop", &json!({})));
                    }
                }

                // If this is a new tool block (not continuing), open it.
                let is_new_block = match &state {
                    StreamState::InToolBlock { index: cur, buffer } => {
                        *cur != index || (!id.is_empty() && buffer.is_empty())
                    }
                    _ => true,
                };
                if is_new_block {
                    out.push(sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": {}
                            }
                        }),
                    ));
                    state = StreamState::InToolBlock {
                        index,
                        buffer: String::new(),
                    };
                }

                // Append arguments fragment to the buffer.
                if let StreamState::InToolBlock { index: _, buffer } = &mut state {
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
            match &state {
                StreamState::InTextBlock => {
                    out.push(sse_event("content_block_stop", &json!({})));
                }
                StreamState::InToolBlock { index, buffer } => {
                    // Emit the aggregated JSON as a single input_json_delta.
                    out.push(sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {"type": "input_json_delta", "partial_json": buffer}
                        }),
                    ));
                    out.push(sse_event("content_block_stop", &json!({})));
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
            state = StreamState::Done;
        }
    }

    (state, out)
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
        let stream = futures::stream::iter(chunks);
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
        let stream = futures::stream::iter(chunks);
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
