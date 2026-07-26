# Agent Switch v0.3 协议互转 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Anthropic→OpenAI 单向协议互转：Claude Code 发 messages 请求经代理转 chat/completions 发到 OpenAI 兼容上游，含完整 tool_use（聚合-再分片流式）。

**Architecture:** core 新增 `proxy/convert.rs`（纯函数转换 + 流式状态机）；forward.rs 在 failover 循环内按 target_protocol 分支转换；proxy_state 加 model_override/target_protocol 两列。

**Tech Stack:** 复用 v0.2 依赖（axum/reqwest/tokio/futures/serde_json）；无新增。

**Spec:** `docs/superpowers/specs/2026-07-26-agent-switch-convert-design.md`

---

## 文件结构

```
crates/core/src/
├── store/db.rs            # 修改：proxy_state 加 model_override/target_protocol 列 + set_routes 扩展
├── proxy/
│   ├── convert.rs         # 新增：请求/响应/流式转换（纯函数 + Stream 状态机）
│   ├── forward.rs         # 修改：failover 循环内按 target_protocol 分支；STRIP_HEADERS 加 anthropic-*
│   └── mod.rs             # 修改：转换路径集成测试
crates/cli/src/main.rs     # 修改：proxy use 加 --model/--target 参数
crates/cli/tests/cli.rs    # 修改：--model/--target 参数测试
```

---

### Task C1: 数据模型迁移 + CLI 参数扩展

**Files:** db.rs、main.rs、cli.rs tests

- [ ] **Step 1: db.rs 迁移** — migrate() 里 proxy_state 建表 SQL 加 `model_override TEXT`、`target_protocol TEXT NOT NULL DEFAULT 'chat'`；对已存在的旧表，用 PRAGMA table_info 检查列缺失后 ALTER TABLE ADD COLUMN（幂等）。set_routes 签名扩展为 `set_routes(&[String], model_override: Option<&str>, target_protocol: &str)`；get_routes 返回 `(Vec<String>, Option<String>, String)`。
- [ ] **Step 2: 写失败测试** — proxy_routes_crud 扩展：set_routes 带 model+target，get_routes 断言三者；旧 db（无新列）迁移后默认 target=chat、model_override=None
- [ ] **Step 3: 实现 + 验证** — `cargo test -p agent-switch-core` 全绿（v0.2 的 proxy_routes_crud 测试需同步更新签名）
- [ ] **Step 4: CLI main.rs** — ProxyCmd::Use 加 `--model: Option<String>`、`--target: Option<String>`（value_parser chat|messages，默认 chat）；调用 set_routes 传新参数；proxy status 打印 model_override + target_protocol
- [ ] **Step 5: CLI 测试** — proxy_use_with_model_and_target：add yy → `proxy use yy --model gpt-5.6-luna --target chat` → status 显示 model+target
- [ ] **Step 6: 提交** — `git add -A && git commit -m "feat(core): proxy_state model_override/target_protocol columns + CLI flags"`

---

### Task C2: 请求侧转换（Anthropic → OpenAI chat）

**Files:** Create `crates/core/src/proxy/convert.rs`；Modify `lib.rs`（proxy/mod.rs 加 `pub mod convert;`）

行为定义（见 spec §3 请求侧）：
- `request_anthropic_to_openai(body: &[u8], model_override: Option<&str>) -> Result<Bytes>`
- system（字符串或 block 数组）→ messages 首条 role=system
- messages[].content blocks：text→content 字符串/part；image→image_url part（data URI 透传）；tool_use（assistant）→ tool_calls；tool_result（user）→ role=tool 消息
- tools[]（input_schema）→ tools[]（parameters）
- tool_choice：auto→"auto"；any→"required"；tool+name→{type:function,function:{name}}
- max_tokens/temperature/top_p 透传；stream 透传
- model：override 有值则替换，否则透传原 model
- 丢弃 thinking blocks

- [ ] **Step 1: 写失败测试**（convert.rs 内 `#[cfg(test)]`，纯函数测试，无网络）

```rust
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
    // assistant message 应有 tool_calls
    let asst = &v["messages"][1];
    assert_eq!(asst["role"], "assistant");
    assert_eq!(asst["tool_calls"][0]["id"], "t1");
    assert_eq!(asst["tool_calls"][0]["function"]["name"], "ls");
    assert_eq!(asst["tool_calls"][0]["function"]["arguments"], r#"{"path":"/"}"#);
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
```

- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 实现** — 纯 serde_json 操作；content block 遍历按 type 分派；tool_use 的 input 序列化为 JSON 字符串放 arguments；tool_result 拆成独立 tool message
- [ ] **Step 4: 运行确认通过** — `cargo test -p agent-switch-core convert` 5 passed
- [ ] **Step 5: 提交** — `git add -A && git commit -m "feat(core): anthropic→openai request conversion"`

---

### Task C3: 响应侧转换（非流式 + 流式状态机）

**Files:** Modify `crates/core/src/proxy/convert.rs`

行为定义（见 spec §3 响应侧）：

非流式 `response_openai_to_anthropic(body: &[u8], req_model: &str) -> Result<Bytes>`：
- choices[0].message.content → content[0] text block
- choices[0].message.tool_calls[] → content[] tool_use blocks（input = arguments JSON 解析对象，失败则 {}）
- finish_reason: stop→end_turn, length→max_tokens, tool_calls→tool_use
- usage: prompt_tokens→input_tokens, completion_tokens→output_tokens
- 顶层 id/model（用 req_model）/role:"assistant"/type:"message"

流式 `stream_openai_to_anthropic(upstream: impl Stream<Item=Bytes>, req_model: String) -> impl Stream<Item=Bytes>`：
状态机（状态：Initial / InTextBlock / InToolBlock{index} / Done）：
1. 首个 chunk 含 role → 发 message_start{message:{id,model:req_model,role:assistant,usage:{input_tokens:0}}} + 进入 InTextBlock
2. delta.content 文本 → 若不在 text block 则先发 content_block_start{text,index:0}；发 content_block_delta{text_delta}
3. delta.tool_calls[i] 出现 → 若在 text block 先 content_block_stop；发 content_block_start{tool_use,index:N,id,name,input:{}}；按 i 聚合 arguments 到缓冲[i]
4. tool_call[i] 完成（finish_reason 或下一 index 出现）→ 发 content_block_delta{input_json_delta, partial_json: 完整 JSON}；content_block_stop
5. finish_reason → 末尾 block 的 content_block_stop；message_delta{stop_reason,usage:{output_tokens}}；message_stop

- [ ] **Step 1: 写失败测试**

```rust
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
    assert!(s.contains(r#""text_delta""text":"h""#));
    assert!(s.contains(r#""text_delta""text":"i""#));
    assert!(s.contains("content_block_stop"));
    assert!(s.contains("message_delta"));
    assert!(s.contains("message_stop"));
    assert!(s.contains(r#""output_tokens":2"#));
}

#[tokio::test]
async fn stream_tool_use_aggregation() {
    // tool_calls arguments 分片："{", `"pa`, `th":"/"}`
    let chunks = vec![
        ok_chunk(br#"{"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"ls","arguments":"{"}}]}}]}"#),
        ok_chunk(br#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"pa"}}]}}]}"#),
        ok_chunk(br#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th:/"}}]}}]}"#),
        ok_chunk(br#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":3}}"#),
    ];
    let stream = futures::stream::iter(chunks);
    let out_bytes = collect_stream(stream_openai_to_anthropic(stream, "m".into())).await;
    let s = String::from_utf8(out_bytes).unwrap();
    // 应有 content_block_start tool_use
    assert!(s.contains(r#""type":"tool_use""id":"t1""name":"ls""#));
    // 聚合后完整 JSON 一次发出
    assert!(s.contains(r#""input_json_delta""partial_json":"{\"path\":\"/\"}""#));
    assert!(s.contains("content_block_stop"));
    assert!(s.contains(r#""stop_reason":"tool_use""#));
}
```

- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 实现** — 非流式纯 serde_json；流式用 futures::stream::unfold 或 async fn + yield 风格的状态机；ok_chunk/collect_stream 放测试辅助
- [ ] **Step 4: 运行确认通过** — `cargo test -p agent-switch-core convert` 全绿
- [ ] **Step 5: 提交** — `git add -A && git commit -m "feat(core): openai→anthropic response + streaming conversion"`

---

### Task C4: forward 整合 + 端到端 + 收尾

**Files:** Modify `proxy/forward.rs`、`proxy/mod.rs`；可能微调 `testsupport.rs`

行为定义：
- STRIP_HEADERS 加入 `anthropic-version`、`anthropic-beta`
- forward_with_failover 内每个 route：取 target_protocol（从 proxy_state 一起返回）；若 endpoint=="messages" && target_protocol=="chat"：
  - 请求：convert::request_anthropic_to_openai(body, model_override) → 注入 stream_options if stream → 发 {base}/chat/completions
  - 响应非流式：convert::response_openai_to_anthropic(resp_body, req_model) → 回客户端
  - 响应流式：convert::stream_openai_to_anthropic(upstream_stream, req_model) → Body::from_stream，tap 提取 usage（用转换后的 Anthropic SSE）
- 否则走 v0.2 透传

- [ ] **Step 1: 写端到端测试**

```rust
#[tokio::test]
async fn e2e_anthropic_to_openai_text() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
    core.db().set_routes(&["yy".into()], Some("gpt-5.6-luna"), "chat").unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    // mock 返回 OpenAI 格式响应
    up.respond_with(200, r#"{"id":"r1","model":"gpt-x","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#);
    // 客户端发 Anthropic 格式请求
    let (status, body) = http_post_json(&url, "/v1/messages",
        r#"{"model":"claude-sonnet-4-6","max_tokens":100,"messages":[{"role":"user","content":"say ok"}]}"#, "Bearer x").await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(v["content"][0]["text"], "ok");
    assert_eq!(v["stop_reason"], "end_turn");
    // 上游收到的是 OpenAI 格式 + model_override
    let seen = up.last_request().await;
    let seen_v: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(seen_v["model"], "gpt-5.6-luna");
    assert!(seen_v["messages"].is_array());
}

#[tokio::test]
async fn e2e_anthropic_to_openai_streaming_tool_use() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
    core.db().set_routes(&["yy".into()], Some("gpt-5.6-luna"), "chat").unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_sse(vec![
        r#"data: {"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"ls","arguments":"{\"path\":\""}}]}}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"/\"}"}}]}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":4}}"#,
        "data: [DONE]",
    ]);
    let (status, body) = http_post_json(&url, "/v1/messages",
        r#"{"model":"m","max_tokens":100,"stream":true,"messages":[{"role":"user","content":"list files"}]}"#, "Bearer x").await;
    assert_eq!(status, 200);
    assert!(body.contains("message_start"));
    assert!(body.contains(r#""tool_use""id":"t1""name":"ls""#));
    assert!(body.contains(r#""input_json_delta""partial_json":"{\"path\":\"/\"}""#));
    assert!(body.contains("content_block_stop"));
    assert!(body.contains("message_stop"));
}

#[tokio::test]
async fn target_messages_passthrough_regression() {
    // target=messages 时仍走 v0.2 透传，不转换
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
    core.db().set_routes(&["yy".into()], None, "messages").unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_with(200, r#"{"type":"message","content":[{"type":"text","text":"ok"}]}"#);
    let (status, body) = http_post_json(&url, "/v1/messages",
        r#"{"model":"m","max_tokens":1,"messages":[{"role":"user","content":"x"}]}"#, "Bearer x").await;
    assert_eq!(status, 200);
    assert!(body.contains(r#""type":"text""text":"ok""#));
    // 上游收到的原样 Anthropic 请求（未转换）
    assert!(up.last_request().await.body.contains(r#""role":"user""#));
}
```

- [ ] **Step 2: 运行确认失败**
- [ ] **Step 3: 实现** — STRIP_HEADERS 加两项；forward_with_failover 内按 target_protocol 分支；get_routes 返回值扩展传递；流式转换路径不复用 make_tapped_streaming_body 而是新 stream + tap
- [ ] **Step 4: 运行确认通过** — `cargo test --workspace` 全绿
- [ ] **Step 5: clippy + fmt** — `cargo clippy --workspace -- -D warnings`；`cargo fmt`
- [ ] **Step 6: 真实环境冒烟** — `asw proxy use yy --model gpt-5.6-luna`；`asw serve`；`asw use local-proxy --tool claude`；`claude -p "say ok"`；验证 Claude Code 经代理转 OpenAI 上游拿到响应
- [ ] **Step 7: 提交** — `git add -A && git commit -m "feat(core): integrate anthropic→openai conversion in proxy forwarding"`

---

## 完成标准

- `cargo test --workspace` 全绿（v0.2 的 67 + v0.3 新增约 10-12）
- `cargo clippy --workspace -- -D warnings`、`cargo fmt --check` 通过
- 真实环境：Claude Code 经代理发请求，转换到 OpenAI 上游，拿到正确响应（含 tool_use 流式）
