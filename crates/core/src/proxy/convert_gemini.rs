//! Protocol conversion between Google Gemini (`generateContent` /
//! `streamGenerateContent`) and Canonical OpenAI Chat Completions IR.

use crate::error::{CoreError, Result};
use axum::body::Bytes;
use futures::Stream;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;

fn extract_parts_text(parts: &[Value]) -> String {
    let mut out = String::new();
    for p in parts {
        if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
            out.push_str(t);
        }
    }
    out
}

/// Convert a Gemini `generateContent` / `streamGenerateContent` request body
/// into an OpenAI `/v1/chat/completions` request body.
pub fn request_gemini_to_chat(
    body: &[u8],
    model_override: Option<&str>,
    is_stream: bool,
) -> Result<Bytes> {
    let input: Value = serde_json::from_slice(body)?;
    let obj = input
        .as_object()
        .ok_or_else(|| CoreError::Proxy("gemini request body is not a JSON object".into()))?;

    let mut messages: Vec<Value> = Vec::new();

    if let Some(sys) = obj
        .get("systemInstruction")
        .or_else(|| obj.get("system_instruction"))
    {
        if let Some(parts) = sys.get("parts").and_then(|p| p.as_array()) {
            let text = extract_parts_text(parts);
            if !text.is_empty() {
                messages.push(json!({ "role": "system", "content": text }));
            }
        }
    }

    if let Some(contents) = obj.get("contents").and_then(|c| c.as_array()) {
        let mut call_seq: usize = 0;
        for item in contents {
            let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let parts = item
                .get("parts")
                .and_then(|p| p.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);

            if role == "model" {
                let text = extract_parts_text(parts);
                let mut tool_calls: Vec<Value> = Vec::new();
                for p in parts {
                    if let Some(fc) = p.get("functionCall") {
                        let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                        call_seq += 1;
                        tool_calls.push(json!({
                            "id": format!("call_{call_seq}"),
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                            }
                        }));
                    }
                }
                let mut msg = Map::new();
                msg.insert("role".into(), json!("assistant"));
                if !text.is_empty() {
                    msg.insert("content".into(), json!(text));
                } else if tool_calls.is_empty() {
                    msg.insert("content".into(), json!(""));
                } else {
                    msg.insert("content".into(), Value::Null);
                }
                if !tool_calls.is_empty() {
                    msg.insert("tool_calls".into(), Value::Array(tool_calls));
                }
                messages.push(Value::Object(msg));
            } else {
                let mut text_buf = String::new();
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                        text_buf.push_str(t);
                    } else if let Some(fr) = p.get("functionResponse") {
                        if !text_buf.is_empty() {
                            messages.push(json!({ "role": "user", "content": text_buf }));
                            text_buf.clear();
                        }
                        let name = fr.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let resp = fr.get("response").cloned().unwrap_or_else(|| json!({}));
                        let content = if let Some(s) = resp.as_str() {
                            s.to_string()
                        } else {
                            serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into())
                        };
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": name,
                            "content": content,
                        }));
                    }
                }
                if !text_buf.is_empty() || parts.is_empty() {
                    messages.push(json!({ "role": "user", "content": text_buf }));
                }
            }
        }
    }

    let mut out = Map::new();
    if let Some(m) = model_override {
        out.insert("model".into(), json!(m));
    } else if let Some(m) = obj.get("model") {
        out.insert("model".into(), m.clone());
    }
    out.insert("messages".into(), Value::Array(messages));
    out.insert("stream".into(), json!(is_stream));
    if is_stream {
        out.insert("stream_options".into(), json!({ "include_usage": true }));
    }

    if let Some(gc) = obj
        .get("generationConfig")
        .or_else(|| obj.get("generation_config"))
        .and_then(|v| v.as_object())
    {
        if let Some(v) = gc.get("temperature") {
            out.insert("temperature".into(), v.clone());
        }
        if let Some(v) = gc.get("topP").or_else(|| gc.get("top_p")) {
            out.insert("top_p".into(), v.clone());
        }
        if let Some(v) = gc
            .get("maxOutputTokens")
            .or_else(|| gc.get("max_output_tokens"))
        {
            out.insert("max_tokens".into(), v.clone());
        }
        if let Some(v) = gc
            .get("stopSequences")
            .or_else(|| gc.get("stop_sequences"))
        {
            out.insert("stop".into(), v.clone());
        }
    }

    if let Some(tools) = obj.get("tools").and_then(|t| t.as_array()) {
        let mut oai_tools: Vec<Value> = Vec::new();
        for tool in tools {
            if let Some(decls) = tool
                .get("functionDeclarations")
                .or_else(|| tool.get("function_declarations"))
                .and_then(|d| d.as_array())
            {
                for d in decls {
                    let Some(name) = d.get("name").and_then(|n| n.as_str()) else {
                        continue;
                    };
                    let mut func = Map::new();
                    func.insert("name".into(), json!(name));
                    if let Some(desc) = d.get("description") {
                        func.insert("description".into(), desc.clone());
                    }
                    if let Some(params) = d.get("parameters").or_else(|| d.get("parametersJsonSchema"))
                    {
                        func.insert("parameters".into(), params.clone());
                    }
                    oai_tools.push(json!({
                        "type": "function",
                        "function": Value::Object(func),
                    }));
                }
            }
        }
        if !oai_tools.is_empty() {
            out.insert("tools".into(), Value::Array(oai_tools));
        }
    }

    Ok(Bytes::from(serde_json::to_vec(&Value::Object(out))?))
}

/// Convert an OpenAI `/v1/chat/completions` request body into a Gemini
/// `generateContent` / `streamGenerateContent` request body.
pub fn request_chat_to_gemini(body: &[u8], _model_override: Option<&str>) -> Result<Bytes> {
    let input: Value = serde_json::from_slice(body)?;
    let obj = input
        .as_object()
        .ok_or_else(|| CoreError::Proxy("chat request body is not a JSON object".into()))?;

    let mut system_texts: Vec<String> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();

    if let Some(msgs) = obj.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content_text = match msg.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };

            if role == "system" || role == "developer" {
                if !content_text.is_empty() {
                    system_texts.push(content_text);
                }
            } else if role == "assistant" {
                let mut parts: Vec<Value> = Vec::new();
                if !content_text.is_empty() {
                    parts.push(json!({ "text": content_text }));
                }
                if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tcs {
                        if let Some(f) = tc.get("function") {
                            let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let args_str =
                                f.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                            let args: Value =
                                serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
                            parts.push(json!({
                                "functionCall": {
                                    "name": name,
                                    "args": args,
                                }
                            }));
                        }
                    }
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            } else if role == "tool" {
                let name = msg
                    .get("name")
                    .or_else(|| msg.get("tool_call_id"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("tool");
                let resp_val = serde_json::from_str::<Value>(&content_text)
                    .unwrap_or_else(|_| json!({ "result": content_text }));
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": resp_val,
                        }
                    }]
                }));
            } else {
                contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": content_text }],
                }));
            }
        }
    }

    let mut out = Map::new();
    if !system_texts.is_empty() {
        out.insert(
            "systemInstruction".into(),
            json!({ "parts": [{ "text": system_texts.join("\n\n") }] }),
        );
    }
    out.insert("contents".into(), Value::Array(contents));

    let mut gc = Map::new();
    if let Some(v) = obj.get("temperature") {
        gc.insert("temperature".into(), v.clone());
    }
    if let Some(v) = obj.get("top_p") {
        gc.insert("topP".into(), v.clone());
    }
    if let Some(v) = obj
        .get("max_tokens")
        .or_else(|| obj.get("max_completion_tokens"))
    {
        gc.insert("maxOutputTokens".into(), v.clone());
    }
    if let Some(v) = obj.get("stop") {
        let stops = match v {
            Value::String(s) => json!([s]),
            arr => arr.clone(),
        };
        gc.insert("stopSequences".into(), stops);
    }
    if !gc.is_empty() {
        out.insert("generationConfig".into(), Value::Object(gc));
    }

    if let Some(tools) = obj.get("tools").and_then(|t| t.as_array()) {
        let mut decls: Vec<Value> = Vec::new();
        for t in tools {
            let f = t.get("function").unwrap_or(t);
            let Some(name) = f.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let mut decl = Map::new();
            decl.insert("name".into(), json!(name));
            if let Some(d) = f.get("description") {
                decl.insert("description".into(), d.clone());
            }
            if let Some(p) = f.get("parameters") {
                decl.insert("parameters".into(), p.clone());
            }
            decls.push(Value::Object(decl));
        }
        if !decls.is_empty() {
            out.insert("tools".into(), json!([{ "functionDeclarations": decls }]));
        }
    }

    Ok(Bytes::from(serde_json::to_vec(&Value::Object(out))?))
}

fn gemini_usage_to_chat(val: &Value) -> Option<Value> {
    let um = val.get("usageMetadata")?;
    let prompt = um
        .get("promptTokenCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion = um
        .get("candidatesTokenCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cached = um
        .get("cachedContentTokenCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let mut u = json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    });
    if cached > 0 {
        u["prompt_tokens_details"] = json!({ "cached_tokens": cached });
    }
    Some(u)
}

fn map_gemini_finish_reason(reason: Option<&str>, has_tool_calls: bool) -> &'static str {
    if has_tool_calls {
        return "tool_calls";
    }
    match reason {
        Some("MAX_TOKENS") => "length",
        Some("SAFETY") | Some("RECITATION") => "content_filter",
        _ => "stop",
    }
}

/// Convert a non-streaming Gemini `generateContent` response into an OpenAI
/// `/v1/chat/completions` response.
pub fn response_gemini_to_chat(bytes: &[u8], model: &str) -> Result<Bytes> {
    let val: Value = serde_json::from_slice(bytes)?;
    let cand = val
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());

    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(parts) = cand
        .and_then(|c| c.pointer("/content/parts"))
        .and_then(|p| p.as_array())
    {
        for (idx, p) in parts.iter().enumerate() {
            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                content.push_str(t);
            }
            if let Some(fc) = p.get("functionCall") {
                let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(json!({
                    "id": format!("call_{}", idx + 1),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                    }
                }));
            }
        }
    }

    let finish_reason = map_gemini_finish_reason(
        cand.and_then(|c| c.get("finishReason"))
            .and_then(|r| r.as_str()),
        !tool_calls.is_empty(),
    );

    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    if content.is_empty() && !tool_calls.is_empty() {
        msg.insert("content".into(), Value::Null);
    } else {
        msg.insert("content".into(), json!(content));
    }
    if !tool_calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(tool_calls));
    }

    let mut out = json!({
        "id": "chatcmpl-gemini",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": Value::Object(msg),
            "finish_reason": finish_reason,
        }]
    });
    if let Some(u) = gemini_usage_to_chat(&val) {
        out["usage"] = u;
    }

    Ok(Bytes::from(serde_json::to_vec(&out)?))
}

/// Convert a non-streaming OpenAI `/v1/chat/completions` response into a Gemini
/// `generateContent` response.
pub fn response_chat_to_gemini(bytes: &[u8], _model: &str) -> Result<Bytes> {
    let val: Value = serde_json::from_slice(bytes)?;
    let choice = val
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let msg = choice.and_then(|c| c.get("message"));

    let mut parts: Vec<Value> = Vec::new();
    if let Some(text) = msg
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
    {
        parts.push(json!({ "text": text }));
    }
    if let Some(tcs) = msg
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        for tc in tcs {
            if let Some(f) = tc.get("function") {
                let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args_str = f.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
                parts.push(json!({
                    "functionCall": {
                        "name": name,
                        "args": args,
                    }
                }));
            }
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }

    let finish_reason = match choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
    {
        Some("length") => "MAX_TOKENS",
        Some("content_filter") => "SAFETY",
        _ => "STOP",
    };

    let mut out = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": parts,
            },
            "finishReason": finish_reason,
            "index": 0,
        }]
    });

    if let Some(u) = val.get("usage") {
        let prompt = u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        let completion = u
            .get("completion_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cached = u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut um = json!({
            "promptTokenCount": prompt,
            "candidatesTokenCount": completion,
            "totalTokenCount": prompt + completion,
        });
        if cached > 0 {
            um["cachedContentTokenCount"] = json!(cached);
        }
        out["usageMetadata"] = um;
    }

    Ok(Bytes::from(serde_json::to_vec(&out)?))
}

/// Stream converter: Gemini SSE (`streamGenerateContent?alt=sse`) → OpenAI Chat SSE.
pub fn stream_gemini_to_chat<S>(
    upstream: S,
    req_model: String,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static
where
    S: Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    struct State {
        upstream: std::pin::Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>,
        buf: String,
        model: String,
        sent_role: bool,
        tool_idx: usize,
        saw_tool_call: bool,
        done_sent: bool,
        pending: VecDeque<std::io::Result<Bytes>>,
    }

    let init = State {
        upstream: Box::pin(upstream),
        buf: String::new(),
        model: req_model,
        sent_role: false,
        tool_idx: 0,
        saw_tool_call: false,
        done_sent: false,
        pending: VecDeque::new(),
    };

    use futures::StreamExt;
    futures::stream::unfold(init, |mut st| async move {
        loop {
            if let Some(item) = st.pending.pop_front() {
                return Some((item, st));
            }
            match st.upstream.next().await {
                Some(Ok(chunk)) => {
                    st.buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = st.buf.find('\n') {
                        let line = st.buf[..pos].trim_end_matches('\r').to_string();
                        st.buf.drain(..=pos);
                        let Some(data) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let data = data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        let Ok(val) = serde_json::from_str::<Value>(data) else {
                            continue;
                        };
                        let cand = val
                            .get("candidates")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first());
                        if let Some(parts) = cand
                            .and_then(|c| c.pointer("/content/parts"))
                            .and_then(|p| p.as_array())
                        {
                            for p in parts {
                                let mut delta = Map::new();
                                if !st.sent_role {
                                    delta.insert("role".into(), json!("assistant"));
                                    st.sent_role = true;
                                }
                                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                    if !t.is_empty() {
                                        delta.insert("content".into(), json!(t));
                                    }
                                }
                                if let Some(fc) = p.get("functionCall") {
                                    let idx = st.tool_idx;
                                    st.tool_idx += 1;
                                    st.saw_tool_call = true;
                                    let name =
                                        fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                    let args =
                                        fc.get("args").cloned().unwrap_or_else(|| json!({}));
                                    delta.insert(
                                        "tool_calls".into(),
                                        json!([{
                                            "index": idx,
                                            "id": format!("call_{}", idx + 1),
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
                                            }
                                        }]),
                                    );
                                }
                                if !delta.is_empty() {
                                    let frame = json!({
                                        "id": "chatcmpl-gemini",
                                        "object": "chat.completion.chunk",
                                        "model": st.model,
                                        "choices": [{
                                            "index": 0,
                                            "delta": Value::Object(delta),
                                            "finish_reason": Value::Null,
                                        }]
                                    });
                                    st.pending
                                        .push_back(Ok(Bytes::from(format!("data: {frame}\n\n"))));
                                }
                            }
                        }

                        let finish = cand
                            .and_then(|c| c.get("finishReason"))
                            .and_then(|r| r.as_str());
                        let usage = gemini_usage_to_chat(&val);
                        if finish.is_some() || usage.is_some() {
                            let mut frame = json!({
                                "id": "chatcmpl-gemini",
                                "object": "chat.completion.chunk",
                                "model": st.model,
                                "choices": if let Some(fr) = finish {
                                    json!([{
                                        "index": 0,
                                        "delta": {},
                                        "finish_reason": map_gemini_finish_reason(Some(fr), st.saw_tool_call),
                                    }])
                                } else {
                                    json!([])
                                }
                            });
                            if let Some(u) = usage {
                                frame["usage"] = u;
                            }
                            st.pending
                                .push_back(Ok(Bytes::from(format!("data: {frame}\n\n"))));
                        }
                    }
                }
                Some(Err(e)) => return Some((Err(e), st)),
                None => {
                    if !st.done_sent {
                        st.done_sent = true;
                        return Some((Ok(Bytes::from_static(b"data: [DONE]\n\n")), st));
                    }
                    return None;
                }
            }
        }
    })
}

/// Stream converter: OpenAI Chat SSE → Gemini SSE (`streamGenerateContent?alt=sse`).
pub fn stream_chat_to_gemini<S>(
    upstream: S,
    _req_model: String,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static
where
    S: Stream<Item = std::io::Result<Bytes>> + Send + 'static,
{
    struct ToolBuf {
        name: String,
        arguments: String,
    }
    struct State {
        upstream: std::pin::Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>,
        buf: String,
        tool_bufs: std::collections::BTreeMap<usize, ToolBuf>,
        pending: VecDeque<std::io::Result<Bytes>>,
    }

    let init = State {
        upstream: Box::pin(upstream),
        buf: String::new(),
        tool_bufs: std::collections::BTreeMap::new(),
        pending: VecDeque::new(),
    };

    use futures::StreamExt;
    futures::stream::unfold(init, |mut st| async move {
        loop {
            if let Some(item) = st.pending.pop_front() {
                return Some((item, st));
            }
            match st.upstream.next().await {
                Some(Ok(chunk)) => {
                    st.buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = st.buf.find('\n') {
                        let line = st.buf[..pos].trim_end_matches('\r').to_string();
                        st.buf.drain(..=pos);
                        let Some(data) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        if data == "[DONE]" {
                            continue;
                        }
                        let Ok(val) = serde_json::from_str::<Value>(data) else {
                            continue;
                        };
                        let choice = val
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first());
                        let mut parts: Vec<Value> = Vec::new();
                        if let Some(delta) = choice.and_then(|c| c.get("delta")) {
                            if let Some(t) = delta
                                .get("content")
                                .and_then(|c| c.as_str())
                                .filter(|s| !s.is_empty())
                            {
                                parts.push(json!({ "text": t }));
                            }
                            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                                for tc in tcs {
                                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                        as usize;
                                    let entry = st.tool_bufs.entry(idx).or_insert_with(|| ToolBuf {
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                    if let Some(n) = tc
                                        .pointer("/function/name")
                                        .and_then(|n| n.as_str())
                                        .filter(|s| !s.is_empty())
                                    {
                                        entry.name = n.to_string();
                                    }
                                    if let Some(a) =
                                        tc.pointer("/function/arguments").and_then(|a| a.as_str())
                                    {
                                        entry.arguments.push_str(a);
                                    }
                                }
                            }
                        }

                        let finish_reason = choice
                            .and_then(|c| c.get("finish_reason"))
                            .and_then(|r| r.as_str());
                        if finish_reason.is_some() && !st.tool_bufs.is_empty() {
                            let bufs = std::mem::take(&mut st.tool_bufs);
                            for (_, tb) in bufs {
                                let args: Value = serde_json::from_str(&tb.arguments)
                                    .unwrap_or_else(|_| json!({}));
                                parts.push(json!({
                                    "functionCall": {
                                        "name": tb.name,
                                        "args": args,
                                    }
                                }));
                            }
                        }

                        let um = val.get("usage").map(|u| {
                            let prompt =
                                u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                            let completion = u
                                .get("completion_tokens")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            json!({
                                "promptTokenCount": prompt,
                                "candidatesTokenCount": completion,
                                "totalTokenCount": prompt + completion,
                            })
                        });

                        if !parts.is_empty() || finish_reason.is_some() || um.is_some() {
                            let mut cand = Map::new();
                            cand.insert(
                                "content".into(),
                                json!({ "role": "model", "parts": parts }),
                            );
                            if let Some(fr) = finish_reason {
                                cand.insert(
                                    "finishReason".into(),
                                    json!(match fr {
                                        "length" => "MAX_TOKENS",
                                        "content_filter" => "SAFETY",
                                        _ => "STOP",
                                    }),
                                );
                            }
                            let mut frame = json!({ "candidates": [Value::Object(cand)] });
                            if let Some(um) = um {
                                frame["usageMetadata"] = um;
                            }
                            st.pending
                                .push_back(Ok(Bytes::from(format!("data: {frame}\n\n"))));
                        }
                    }
                }
                Some(Err(e)) => return Some((Err(e), st)),
                None => return None,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn request_gemini_to_chat_and_back_roundtrip() {
        let gem_req = br#"{
            "systemInstruction": {"parts": [{"text": "Be concise."}]},
            "contents": [
                {"role": "user", "parts": [{"text": "Weather in Tokyo?"}]},
                {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}}}]},
                {"role": "user", "parts": [{"functionResponse": {"name": "get_weather", "response": {"temp": 22}}}]}
            ],
            "tools": [{"functionDeclarations": [{"name": "get_weather", "description": "Get weather", "parameters": {"type": "object"}}]}],
            "generationConfig": {"temperature": 0.2, "maxOutputTokens": 512}
        }"#;

        let chat_bytes = request_gemini_to_chat(gem_req, Some("gpt-5"), true).unwrap();
        let chat: Value = serde_json::from_slice(&chat_bytes).unwrap();
        assert_eq!(chat["model"], "gpt-5");
        assert_eq!(chat["stream"], true);
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][1]["role"], "user");
        assert_eq!(
            chat["messages"][2]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(chat["messages"][3]["role"], "tool");
        assert_eq!(chat["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(chat["max_tokens"], 512);

        let gem_back_bytes = request_chat_to_gemini(&chat_bytes, None).unwrap();
        let gem_back: Value = serde_json::from_slice(&gem_back_bytes).unwrap();
        assert_eq!(
            gem_back["systemInstruction"]["parts"][0]["text"],
            "Be concise."
        );
        assert_eq!(
            gem_back["contents"][1]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            gem_back["contents"][2]["parts"][0]["functionResponse"]["name"],
            "get_weather"
        );
    }

    #[test]
    fn response_gemini_to_chat_and_chat_to_gemini() {
        let gem_resp = br#"{
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hello!"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "cachedContentTokenCount": 4}
        }"#;
        let chat_bytes = response_gemini_to_chat(gem_resp, "gemini-2.5-pro").unwrap();
        let chat: Value = serde_json::from_slice(&chat_bytes).unwrap();
        assert_eq!(chat["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(chat["choices"][0]["finish_reason"], "stop");
        assert_eq!(chat["usage"]["prompt_tokens"], 10);
        assert_eq!(chat["usage"]["completion_tokens"], 5);
        assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 4);

        let gem_bytes = response_chat_to_gemini(&chat_bytes, "gemini-2.5-pro").unwrap();
        let gem: Value = serde_json::from_slice(&gem_bytes).unwrap();
        assert_eq!(gem["candidates"][0]["content"]["parts"][0]["text"], "Hello!");
        assert_eq!(gem["candidates"][0]["finishReason"], "STOP");
        assert_eq!(gem["usageMetadata"]["promptTokenCount"], 10);
    }

    #[tokio::test]
    async fn stream_chat_to_gemini_emits_candidates_and_usage() {
        let chunks = vec![
            Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hi\"}}]}\n\n",
            )),
            Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
            )),
            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
        ];
        let out: Vec<String> = stream_chat_to_gemini(futures::stream::iter(chunks), "m".into())
            .map(|r| String::from_utf8_lossy(&r.unwrap()).into_owned())
            .collect()
            .await;
        let joined = out.join("");
        assert!(joined.contains("\"text\":\"Hi\""), "{joined}");
        assert!(joined.contains("\"finishReason\":\"STOP\""), "{joined}");
        assert!(joined.contains("\"promptTokenCount\":3"), "{joined}");
    }
}
