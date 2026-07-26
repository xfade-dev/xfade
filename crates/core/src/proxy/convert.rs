//! Protocol conversion: Anthropic messages → OpenAI chat/completions.
//!
//! This module contains pure-function conversions for the request side
//! (Anthropic → OpenAI). The response and streaming conversions live in
//! later tasks; see `docs/superpowers/plans/2026-07-26-agent-switch-convert.md`
//! Task C2 for the scope implemented here.

use crate::error::{CoreError, Result};
use axum::body::Bytes;
use serde_json::{json, Map, Value};

/// Convert an Anthropic `/v1/messages` request body into an OpenAI
/// `/v1/chat/completions` request body.
///
/// Behavior (see spec §3 请求侧):
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
}
