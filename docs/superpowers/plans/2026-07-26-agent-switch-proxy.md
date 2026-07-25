# Agent Switch v0.2 本地代理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `asw serve` 把路由 provider 暴露为本地 OpenAI/Anthropic 兼容端点：热切换、failover+熔断、请求统计。

**Architecture:** core 新增 `proxy` 模块（唯一 async 部分），其余保持同步；CLI `serve` 子命令内创建 tokio runtime。转发=纯透传（仅替换 Authorization + chat 流式注入 stream_options），路由每请求现查 SQLite，熔断为进程内存态。

**Tech Stack:** axum 0.8 · reqwest 0.12 (rustls-tls, stream) · tokio 1 · futures · time 0.3 (rfc3339) · 测试：axum 自建 mock 上游

**Spec:** `docs/superpowers/specs/2026-07-26-agent-switch-proxy-design.md`

---

## 文件结构

```
crates/core/src/
├── store/db.rs            # 修改：proxy_state/request_logs 迁移 + routes CRUD + 日志/统计查询
├── proxy/
│   ├── mod.rs             # ProxyService、Circuit、serve()/build_router()
│   ├── forward.rs         # handle/handle_get、failover 循环、非流式与流式转发、日志落库
│   ├── usage.rs           # usage 提取（非流式 JSON / SSE 尾部）
│   └── testsupport.rs     # #[cfg(test)] MockUpstream / spawn_service / http_post_json / test_core
├── error.rs               # 修改：+Proxy(String)
└── presets.rs             # 修改：+local-proxy 条目
crates/cli/src/main.rs     # 修改：+Serve/Proxy{Use,Status,Clear}/Stats
crates/cli/tests/cli.rs    # 修改：+proxy 命令组集成测试
```

依赖（workspace `[workspace.dependencies]` 追加）：`tokio = { version = "1", features = ["rt-multi-thread", "macros"] }`、`axum = "0.8"`、`reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream", "json"] }`、`futures = "0.3"`、`time = { version = "0.3", features = ["formatting", "parsing"] }`。core 五个全加（workspace=true）；CLI 加 `tokio`。

service.rs 补访问器：`pub fn db(&self) -> &Database`（proxy 与测试需要）。

---

### Task P1: 依赖 + 数据库迁移与统计查询

**Files:** 三个 Cargo.toml、`crates/core/src/store/db.rs`

- [ ] **Step 1: 加依赖**（上述三处），`cargo build` 确认可编译
- [ ] **Step 2: 写失败测试**（db.rs 测试模块追加）

```rust
#[test]
fn proxy_routes_crud() {
    let db = Database::open_memory().unwrap();
    assert!(db.get_routes().unwrap().is_none());
    db.set_routes(&["yy".into(), "bak".into()]).unwrap();
    assert_eq!(db.get_routes().unwrap().unwrap(), vec!["yy".to_string(), "bak".to_string()]);
    db.clear_routes().unwrap();
    assert!(db.get_routes().unwrap().is_none());
}

#[test]
fn request_log_and_stats() {
    let db = Database::open_memory().unwrap();
    db.insert_request_log(&RequestLog {
        ts: "2026-07-26T10:00:00Z".into(), endpoint: "chat".into(),
        model: Some("gpt-5.6-luna".into()), provider_id: "yy".into(),
        status: 200, prompt_tokens: 100, completion_tokens: 50, duration_ms: 800, error: None,
    }).unwrap();
    db.insert_request_log(&RequestLog {
        ts: "2026-07-26T11:00:00Z".into(), endpoint: "messages".into(),
        model: Some("claude-sonnet-4-6".into()), provider_id: "yy".into(),
        status: 429, prompt_tokens: 0, completion_tokens: 0, duration_ms: 120,
        error: Some("rate limited".into()),
    }).unwrap();
    let rows = db.stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Provider).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].requests, 2);
    assert_eq!(rows[0].prompt_tokens, 100);
    assert_eq!(rows[0].errors, 1);
    let by_model = db.stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Model).unwrap();
    assert_eq!(by_model.len(), 2);
}
```

- [ ] **Step 3: 运行确认失败** — `cargo test -p agent-switch-core proxy` FAIL
- [ ] **Step 4: 实现**

db.rs 新类型（impl 外）：`RequestLog { ts, endpoint, model: Option<String>, provider_id, status, prompt_tokens, completion_tokens, duration_ms, error: Option<String> }`（数值均 i64）、`StatsGroupBy { Provider, Model }`、`StatsRow { group, requests, prompt_tokens, completion_tokens, errors, avg_duration_ms }`。

migrate() 追加：

```sql
CREATE TABLE IF NOT EXISTS proxy_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    routes TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS request_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL, endpoint TEXT NOT NULL, model TEXT,
    provider_id TEXT NOT NULL, status INTEGER NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL, error TEXT
);
CREATE INDEX IF NOT EXISTS idx_request_logs_ts ON request_logs(ts);
```

方法：`set_routes(&[String])`（INSERT..ON CONFLICT(id) DO UPDATE，updated_at 用 time crate Rfc3339 now）、`get_routes() -> Result<Option<Vec<String>>>`、`clear_routes()`、`insert_request_log(&RequestLog)`、`stats_since(since_ts, by) -> Result<Vec<StatsRow>>`（GROUP BY provider_id 或 COALESCE(model,'(unknown)')，`SUM(CASE WHEN status>=400 THEN 1 ELSE 0 END)` 为 errors，`CAST(AVG(duration_ms) AS INTEGER)`，ORDER BY 2 DESC；SUM 返 Option<i64> 处理 NULL）。

- [ ] **Step 5: 运行确认通过** — `cargo test -p agent-switch-core` → 42 passed（40+2）
- [ ] **Step 6: 提交** — `git add -A && git commit -m "feat(core): proxy_state/request_logs tables with stats queries"`

---

### Task P2: mock 上游 + 非流式透传 + auth 替换 + 统计落库

**Files:** Create `proxy/{mod,forward,usage,testsupport}.rs`；Modify `lib.rs`（+`pub mod proxy;`）、`error.rs`（+`Proxy(String)`）、`service.rs`（+`pub fn db()`）

行为定义：
- `ProxyService { core, client: reqwest::Client, circuits: Arc<Mutex<HashMap<String, Circuit>>>, auth_token: Option<String> }`；`Circuit { fails: u32, cooldown_until: Option<Instant> }`；常量 `FAIL_THRESHOLD=3`、`COOLDOWN_SECS=60`。方法：`new/with_auth_token/core()/circuit_open/record_success(清零移除)/record_failure(达阈值置冷却)/circuit_status()/serve(host,port)`
- Router：`GET /health`（"ok"）、`POST /v1/chat/completions|/v1/responses|/v1/messages`（forward::handle）、`GET /v1/models`（forward::handle_get）；`DefaultBodyLimit::max(32MB)`
- handle 流程：计时 → 读全 body → auth-token 校验（设了就必须 `Bearer <t>` 否则 401）→（P3 注入点，本任务透传）→ 提取 model → forward_with_failover
- failover 循环：`get_routes()` 空 → 503 提示文案；逐 route：熔断中 skip → 取 provider（`core.list(None)` 找第一个 id 匹配项）+ keyring key（official 无 key 则不带 Authorization）→ try_forward：
  - 成功（2xx/3xx）→ record_success、写日志、返回响应
  - 429/5xx/连接错误/超时 → record_failure、写日志（status 或 0）、记 last_err、继续下一个
  - 其他 4xx → 原样返回（不熔断不 failover），写日志
  - 全部失败 → 最后错误的状态码+文本；无可用 → 503 "no usable provider"
- try_forward：method（models=GET，其余 POST），url = base 去尾 `/` + 端点路径（chat→/chat/completions，responses→/responses，messages→/messages，models→/models）；headers 复制入站但剔除 `host/content-length/authorization/connection/transfer-encoding`，加 `Authorization: Bearer <key>`；非流式读全响应 bytes 返回
- usage.rs：`Usage { prompt_tokens, completion_tokens }`（Default）；`from_json(bytes)` 兼容 OpenAI（prompt/completion_tokens）与 Anthropic（input/output_tokens）
- 日志：每请求写 request_logs（endpoint/model/provider 实际命中/status/prompt/completion/duration_ms/error 摘要）
- testsupport.rs（`#[cfg(test)]`）：`MockUpstream`（axum 起 127.0.0.1:0；`url()`；`respond_with(status,body)` / `respond_sequence(Vec<(status,body)>)` 按调用序消费，耗尽重复最后一个；`last_request() -> RecordedRequest{method,path,authorization,body}`；`call_count()`）、`spawn_service(ProxyService) -> String`（bind :0 拿端口后 spawn）、`http_post_json(url,path,body,auth) -> (u16,String)`、`test_core(dir, &[(id,base_url,key)]) -> Core`

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn forwards_chat_with_replaced_auth() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "sk-real")]);
    core.db().set_routes(&["yy".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_with(200, r#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#);
    let (status, body) = http_post_json(&url, "/v1/chat/completions",
        r#"{"model":"gpt-x","messages":[]}"#, "Bearer sk-whatever").await;
    assert_eq!(status, 200);
    assert!(body.contains("prompt_tokens"));
    let seen = up.last_request().await;
    assert_eq!(seen.authorization, "Bearer sk-real");
    assert!(seen.body.contains("\"model\":\"gpt-x\""));
}

#[tokio::test]
async fn no_route_returns_503() {
    let dir = tempfile::tempdir().unwrap();
    let core = test_core(&dir, &[]);
    let url = spawn_service(ProxyService::new(core)).await;
    let (status, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
    assert_eq!(status, 503);
}

#[tokio::test]
async fn upstream_4xx_no_failover() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let up2 = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
    core.db().set_routes(&["yy".into(), "bak".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_with(403, r#"{"error":"denied"}"#);
    let (status, body) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
    assert_eq!(status, 403);
    assert!(body.contains("denied"));
    assert_eq!(up2.call_count().await, 0);
}

#[tokio::test]
async fn auth_token_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
    core.db().set_routes(&["yy".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core).with_auth_token(Some("t1".into()))).await;
    let (s1, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer wrong").await;
    assert_eq!(s1, 401);
    up.respond_with(200, "{}");
    let (s2, _) = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer t1").await;
    assert_eq!(s2, 200);
}

#[tokio::test]
async fn non_stream_usage_logged() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
    core.db().set_routes(&["yy".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_with(200, r#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#);
    let _ = http_post_json(&url, "/v1/chat/completions", r#"{"model":"m1","messages":[]}"#, "Bearer x").await;
    let stats = core.db().stats_since("2020-01-01T00:00:00Z", crate::store::db::StatsGroupBy::Provider).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].prompt_tokens, 10);
    assert_eq!(stats[0].completion_tokens, 5);
}
```

- [ ] **Step 2: 运行确认失败** — `cargo test -p agent-switch-core proxy` FAIL
- [ ] **Step 3: 实现**（行为定义如上，逐条落实）
- [ ] **Step 4: 运行确认通过** — `cargo test -p agent-switch-core` → 47 passed（42+5）
- [ ] **Step 5: 提交** — `git add -A && git commit -m "feat(core): proxy non-stream forwarding with auth replacement and stats"`

---

### Task P3: 流式透传 + usage 尾部提取 + failover/熔断

**Files:** Modify `proxy/forward.rs`、`proxy/usage.rs`、`proxy/testsupport.rs`（MockUpstream 增加 `respond_sse(Vec<&str>)`：content-type text/event-stream，各行拼 `\n\n` 结尾）

行为定义：
- 流式判定：以上游响应 content-type 含 text/event-stream 为准；请求 stream 字段仅用于注入判定
- 流式转发：resp.bytes_stream() → Body::from_stream，status 与 content-type 透传；流上套 tap：逐 chunk 转发同时追加到尾部环形缓冲（~8KB），流结束后 usage::from_sse_tail 提取并写日志
- usage::from_sse_tail(endpoint, tail)：逐行解析 `data: <json>`，含 prompt_tokens|input_tokens 更新 prompt，含 completion_tokens|output_tokens 更新 completion（message_delta 的 output_tokens 为累计值取最后）；取不到记 0
- stream_options 注入：endpoint==chat 且请求 JSON stream==true 且无 stream_options 键 → 加 {"stream_options":{"include_usage":true}}（仅注入场景重写 body）
- failover 全链路：429 → 下一个 route；主连续 3 次 failover 类失败 → 熔断 skip 60s

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn stream_chat_passthrough_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
    core.db().set_routes(&["yy".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_sse(vec![
        r#"data: {"choices":[{"delta":{"content":"o"}}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":1}}"#,
        "data: [DONE]",
    ]);
    let (status, body) = http_post_json(&url, "/v1/chat/completions",
        r#"{"model":"m","messages":[],"stream":true}"#, "Bearer x").await;
    assert_eq!(status, 200);
    assert!(body.contains("\"content\":\"o\""));
    assert!(body.contains("[DONE]"));
    let stats = core.db().stats_since("2020-01-01T00:00:00Z", crate::store::db::StatsGroupBy::Provider).unwrap();
    assert_eq!(stats[0].prompt_tokens, 7);
    assert_eq!(stats[0].completion_tokens, 1);
    assert!(up.last_request().await.body.contains("include_usage"));
}
```

```rust
#[tokio::test]
async fn failover_on_429_then_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let up2 = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
    core.db().set_routes(&["yy".into(), "bak".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_with(429, "rate limited");
    up2.respond_with(200, r#"{"usage":{"prompt_tokens":1,"completion_tokens":1}}"#);
    let (status, _) = http_post_json(&url, "/v1/chat/completions", r#"{"model":"m","messages":[]}"#, "Bearer x").await;
    assert_eq!(status, 200);
    assert_eq!(up.call_count().await, 1);
    assert_eq!(up2.call_count().await, 1);
    let stats = core.db().stats_since("2020-01-01T00:00:00Z", crate::store::db::StatsGroupBy::Provider).unwrap();
    assert_eq!(stats.iter().find(|s| s.group == "yy").unwrap().errors, 1);
    assert_eq!(stats.iter().find(|s| s.group == "bak").unwrap().requests, 1);
}

#[tokio::test]
async fn circuit_opens_after_threshold_and_skips() {
    let dir = tempfile::tempdir().unwrap();
    let up = MockUpstream::spawn().await;
    let up2 = MockUpstream::spawn().await;
    let core = test_core(&dir, &[("yy", &up.url(), "k1"), ("bak", &up2.url(), "k2")]);
    core.db().set_routes(&["yy".into(), "bak".into()]).unwrap();
    let url = spawn_service(ProxyService::new(core)).await;
    up.respond_with(500, "boom");
    up2.respond_with(200, "{}");
    for _ in 0..3 {
        let _ = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
    }
    let calls_after_3 = up.call_count().await;
    let _ = http_post_json(&url, "/v1/chat/completions", "{}", "Bearer x").await;
    assert_eq!(up.call_count().await, calls_after_3);
    assert!(up2.call_count().await >= 4);
}
```

- [ ] **Step 2: 运行确认失败** — FAIL
- [ ] **Step 3: 实现**（行为定义如上）
- [ ] **Step 4: 运行确认通过** — `cargo test -p agent-switch-core` → 50 passed（47+3）
- [ ] **Step 5: 提交** — `git add -A && git commit -m "feat(core): proxy streaming passthrough, failover and circuit breaker"`

---

### Task P4: CLI serve/proxy/stats + local-proxy 预设

**Files:** Modify `crates/cli/src/main.rs`、`crates/core/src/presets.rs`、`crates/cli/tests/cli.rs`

presets.rs 每工具 official 之后追加：`Preset { id: "local-proxy", label: "本地代理 (asw serve)", base_url: Some(...), extra: Value::Null }`（claude: `http://127.0.0.1:24860`；codex/opencode: `http://127.0.0.1:24860/v1`）。

main.rs 新子命令：

```rust
Serve {
    #[arg(long, default_value = "24860")] port: u16,
    #[arg(long, default_value = "127.0.0.1")] host: String,
    #[arg(long)] auth_token: Option<String>,
},
Proxy { #[command(subcommand)] cmd: ProxyCmd },   // Use { names: Vec<String> } / Status / Clear
Stats {
    #[arg(long, default_value = "7d")] since: String,
    #[arg(long, value_parser = ["provider", "model"])] by: Option<String>,
},
```

实现要点：
- `Serve`：`tokio::runtime::Runtime::new().unwrap().block_on(async { ProxyService::new(build_core()?).with_auth_token(auth_token).serve(&host, port).await })`
- `Proxy Use`：names 非空校验、每个 name 须在 core.list(None) 存在否则报错；set_routes；打印 主→备
- `Proxy Status`：读 db routes 打印（空则 "(none)"）；熔断状态提示"仅在 serve 进程内可见"
- `Proxy Clear`：clear_routes()
- `Stats`：since 仅支持 `<N>d`（now−N 天，time crate 算 RFC3339 前缀）；默认 by=provider；表格列 group/requests/tokens/errors/avg_ms

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn proxy_use_status_clear() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["add", "--tool", "codex", "--name", "yy", "--base-url", "http://x", "--key", "k"])
        .assert().success();
    asw(home.path())
        .args(["proxy", "use", "yy"])
        .assert().success()
        .stdout(predicate::str::contains("yy"));
    asw(home.path())
        .args(["proxy", "status"])
        .assert().success()
        .stdout(predicate::str::contains("yy"));
    asw(home.path())
        .args(["proxy", "use", "nope"])
        .assert().failure();
    asw(home.path()).args(["proxy", "clear"]).assert().success();
}

```rust
#[test]
fn stats_empty_ok() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path()).args(["stats"]).assert().success();
}

#[test]
fn presets_include_local_proxy() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["presets"])
        .assert().success()
        .stdout(predicate::str::contains("local-proxy"));
}

#[test]
fn serve_help_lists_options() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["serve", "--help"])
        .assert().success()
        .stdout(predicate::str::contains("--port").and(predicate::str::contains("--auth-token")));
}
```

- [ ] **Step 2: 运行确认失败** — `cargo test -p asw` FAIL（子命令不存在）
- [ ] **Step 3: 实现**（上述 presets + 子命令 + 要点）
- [ ] **Step 4: 运行确认通过** — `cargo test --workspace` → 54 passed（50 core + 4 cli 新增）
- [ ] **Step 5: 提交** — `git add -A && git commit -m "feat(cli): serve/proxy/stats commands and local-proxy presets"`

---

### Task P5: clippy/fmt + 真实环境冒烟 + README

- [ ] **Step 1: clippy + fmt** — `cargo clippy --workspace -- -D warnings` 零告警；`cargo fmt`
- [ ] **Step 2: 真实环境冒烟**（报告输出）

```bash
cargo build --release
./target/release/asw proxy use yy
./target/release/asw serve --port 24860 &
curl -X POST http://127.0.0.1:24860/v1/chat/completions \
  -H "Authorization: Bearer any" -H "Content-Type: application/json" \
  -d '{"model":"gpt-5.6-luna","messages":[{"role":"user","content":"hi"}],"max_tokens":16}'
./target/release/asw stats
```

- [ ] **Step 3: README**（项目根新建，四节各不超过 15 行：简介/安装/命令速查/代理模式上手）
- [ ] **Step 4: 提交** — `git add -A && git commit -m "docs: readme with proxy mode quickstart"`

---

## 完成标准

- `cargo test --workspace` 全绿（约 54：core 50 + cli 12）
- `cargo clippy --workspace -- -D warnings`、`cargo fmt --check` 通过
- 真实中转冒烟：curl 经代理拿到正常响应；`asw stats` 有聚合数据；`asw proxy use` 切换后下一个 curl 即走新 provider
