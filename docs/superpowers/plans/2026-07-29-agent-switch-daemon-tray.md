# Agent Switch 代理常驻 + 托盘 Implementation Plan（v0.5 / Phase 5a）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** CLI daemon（macOS launchd 常驻 + 自启 + 崩溃重启）+ 代理 `GET /__asw/status` 管理端点 + GUI 托盘（关窗到托盘、含每工具快速切换、定时刷新）控制同一 daemon。

**Architecture:** daemon = launchd 托管的 `asw serve --foreground` 进程；CLI `serve install/uninstall/status/stop` 写 plist + daemon.json + launchctl；代理新增 `/__asw/status`（auth 共享 layer 校验）供 CLI/GUI 查询；GUI 的 `proxy_ctl` 内嵌 spawn 替换为 `daemon_ctl`（HTTP 查询 + 子进程 install/uninstall），tauri command 名不变；GUI 托盘 + autostart 插件。

**Tech Stack:** Rust（agent-switch-core、axum middleware、plist 写入、launchctl 子进程）、Tauri v2（tray、tauri-plugin-autostart）、React（Proxy 页前端不变）。

**Spec:** `docs/superpowers/specs/2026-07-29-agent-switch-daemon-tray-design.md`

---

## 文件结构总览

**core 修改：**
- `crates/core/src/proxy/mod.rs` — `ProxyService` 增 `host/port` 字段 + `with_host_port` builder；`serve`/`serve_with_shutdown` 用自身字段；`build_router` 加 `/__asw/status` + auth 共享 layer；新增 `status` 模块/DTO。
- `crates/core/src/proxy/forward.rs` — 移除 `handle`/`handle_get` 内联 auth 校验（改由 layer 统一）。
- `crates/core/src/proxy/status.rs`（新）— `/__asw/status` handler + `StatusDto`/`CircuitDto` + `circuit_to_dto`。
- `crates/core/src/proxy/auth.rs`（新）— `auth_guard` axum middleware。
- `crates/core/src/daemon.rs`（新）— `DaemonConfig` 类型 + daemon.json read/write（CLI 与 GUI 共用）。
- 调用点：`crates/cli/src/main.rs`（Serve）、`crates/core/src/proxy/testsupport.rs`、`crates/core/src/proxy/mod.rs` 测试、`crates/gui/src-tauri/src/proxy_ctl.rs`。

**CLI 修改：**
- `crates/cli/src/main.rs` — `Cmd::Serve` 改为 `Serve { cmd: Option<ServeCmd> }`；新增 `ServeCmd::{Foreground,Install,Uninstall,Stop,Status}`；`daemon.rs`（新）plist/daemon.json/launchctl 逻辑。

**GUI 修改：**
- `crates/gui/src-tauri/src/daemon_ctl.rs`（新，替换 `proxy_ctl.rs`）— HTTP status 查询 + install/uninstall 子进程。
- `crates/gui/src-tauri/src/commands.rs` — `proxy_start/stop/status` 改委托 `daemon_ctl`；`dto.rs` 的 `circuit_dto` 改 re-export core。
- `crates/gui/src-tauri/src/lib.rs` — 托盘 + 关窗到托盘 + autostart 插件注册；`state.rs` 改用 `daemon_ctl`。
- `crates/gui/src-tauri/Cargo.toml` — 加 `tauri-plugin-autostart` + `reqwest = { workspace = true }`。
- `crates/gui/src-tauri/src/tray.rs`（新）— 托盘菜单构建 + 定时刷新。

---

## Task 1: ProxyService 增 host/port 字段 + builder

**Files:**
- Modify: `crates/core/src/proxy/mod.rs:27-95`
- Modify: `crates/core/src/proxy/testsupport.rs`（无改动，仅确认）
- Modify: `crates/cli/src/main.rs`（Serve 调用点）
- Modify: `crates/gui/src-tauri/src/proxy_ctl.rs`（调用点）

- [ ] **Step 1: 写失败测试**

`crates/core/src/proxy/mod.rs` 测试模块追加：

```rust
    #[tokio::test]
    async fn serve_with_shutdown_uses_configured_host_port() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            ProxyService::new(core)
                .with_host_port("127.0.0.1", port)
                .serve_with_shutdown(async move { let _ = rx.await; })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p agent-switch-core --lib serve_with_shutdown_uses_configured_host_port`
Expected: 编译失败（`with_host_port` 不存在 / `serve_with_shutdown` 参数不匹配）

- [ ] **Step 3: 实现**

`crates/core/src/proxy/mod.rs`：

```rust
pub struct ProxyService {
    pub core: Core,
    pub client: reqwest::Client,
    pub circuits: Arc<Mutex<HashMap<String, Circuit>>>,
    pub auth_token: Option<String>,
    pub host: String,
    pub port: u16,
}

impl ProxyService {
    pub fn new(core: Core) -> Self {
        Self {
            core,
            client: reqwest::Client::new(),
            circuits: Arc::new(Mutex::new(HashMap::new())),
            auth_token: None,
            host: "127.0.0.1".to_string(),
            port: 0,
        }
    }

    pub fn with_host_port(mut self, host: impl Into<String>, port: u16) -> Self {
        self.host = host.into();
        self.port = port;
        self
    }

    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token;
        self
    }

    // circuit_open / record_success / record_failure / circuit_status 不变

    pub async fn serve_with_shutdown(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port); // 先拷出，build_router 会消耗 self
        let app = self.build_router();
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| crate::error::CoreError::Proxy(format!("bind {addr}: {e}")))?;
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| crate::error::CoreError::Proxy(format!("serve: {e}")))?;
        Ok(())
    }

    pub async fn serve(self) -> Result<()> {
        self.serve_with_shutdown(std::future::pending::<()>()).await
    }
}
```

更新调用点：
- `crates/core/src/proxy/mod.rs` 既有 `serve_with_shutdown_stops_and_releases_port` 测试：`ProxyService::new(core2).serve_with_shutdown("127.0.0.1", port, async move{...})` → `ProxyService::new(core2).with_host_port("127.0.0.1", port).serve_with_shutdown(async move{...})`。
- `crates/cli/src/main.rs` `Cmd::Serve`：`ProxyService::new(core).with_auth_token(auth_token).serve(&host, port)` → `ProxyService::new(core).with_host_port(&host, port).with_auth_token(auth_token).serve()`。
- `crates/gui/src-tauri/src/proxy_ctl.rs` `start`：`ProxyService::new(core).with_auth_token(...)` → 加 `.with_host_port(&host, port)` 在 with_auth_token 前；`serve_with_shutdown(&host_for_task, port, ...)` → `.with_host_port(&host_for_task, port).serve_with_shutdown(async move{...})`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全绿（含新测试）

- [ ] **Step 5: clippy/fmt + 提交**

```bash
cargo clippy --all-targets -- -D warnings && cargo fmt --all
git add -A && git commit -m "feat(core): ProxyService host/port fields + with_host_port builder"
```

---

## Task 2: auth_guard 共享 middleware

**Files:**
- Create: `crates/core/src/proxy/auth.rs`
- Modify: `crates/core/src/proxy/mod.rs`（mod 声明 + build_router 挂 layer）
- Modify: `crates/core/src/proxy/forward.rs:766-798`（移除内联 auth）

- [ ] **Step 1: 写失败测试（已有覆盖）**

现有 `auth_token_enforced` 测试（mod.rs）已验证 /v1/* 鉴权；新增 `/__asw/status` 鉴权测试在 Task 3。本步先保证现有测试不回归。

- [ ] **Step 2: 实现 auth.rs**

```rust
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use super::ProxyService;

pub async fn auth_guard(
    State(svc): State<Arc<ProxyService>>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(token) = &svc.auth_token {
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth != format!("Bearer {token}") {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }
    next.run(req).await
}
```

- [ ] **Step 3: build_router 挂 layer（mod.rs）**

`build_router` 改为：`/health` 单独无保护；`/v1/*` 与 `/__asw/status`（Task 3 加）挂 `from_fn_with_state(svc.clone(), auth_guard)` layer。由于 `build_router(self)` 消耗 self，需先 clone state：

```rust
pub fn build_router(self) -> Router {
    let state = Arc::new(self);
    let protected = Router::new()
        .route("/v1/chat/completions", post(forward::handle))
        .route("/v1/responses", post(forward::handle))
        .route("/v1/messages", post(forward::handle))
        .route("/v1/models", get(forward::handle_get))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::auth_guard));
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(protected)
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state)
}
```

`mod.rs` 顶部加 `mod auth;`（pub mod 视可见性）+ `use axum::middleware`。

- [ ] **Step 4: 移除 forward.rs 内联 auth**

`forward.rs` `handle`（:768-778）与 `handle_get`（:787-797）删除 `if let Some(token) = &svc.auth_token { ... }` 块（layer 已接管）。`headers: HeaderMap` 参数若不再用则保留（forward 仍需读 headers 转发上游）。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p agent-switch-core --lib`
Expected: 全绿（`auth_token_enforced` 等仍通过，/health 仍免鉴权）

- [ ] **Step 6: clippy/fmt + 提交**

```bash
cargo clippy --all-targets -- -D warnings && cargo fmt --all
git add -A && git commit -m "refactor(core): extract auth_guard middleware, ungate /health"
```

---

## Task 3: /__asw/status 端点 + DTO 下沉

**Files:**
- Create: `crates/core/src/proxy/status.rs`
- Modify: `crates/core/src/proxy/mod.rs`（mod 声明 + build_router 加路由）
- Modify: `crates/gui/src-tauri/src/dto.rs`（circuit_dto 改 re-export）

- [ ] **Step 1: 写失败测试**

`crates/core/src/proxy/mod.rs` 测试模块追加：

```rust
    #[tokio::test]
    async fn status_endpoint_returns_state() {
        let dir = tempfile::tempdir().unwrap();
        let up = MockUpstream::spawn().await;
        let core = test_core(&dir, &[("yy", &up.url(), "k1")]);
        core.db().set_routes(&["yy".into()], Some("gpt-x"), "chat").unwrap();
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            ProxyService::new(core)
                .with_host_port("127.0.0.1", port)
                .serve_with_shutdown(async move { let _ = rx.await; })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let v: serde_json::Value = reqwest::get(format!("http://127.0.0.1:{port}/__asw/status"))
            .await.unwrap().json().await.unwrap();
        assert_eq!(v["running"], true);
        assert_eq!(v["port"], port);
        assert_eq!(v["auth_enabled"], false);
        assert_eq!(v["routes"][0], "yy");
        assert_eq!(v["model_override"], "gpt-x");
        assert_eq!(v["target_protocol"], "chat");
        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn status_endpoint_requires_auth_token() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            ProxyService::new(core)
                .with_host_port("127.0.0.1", port)
                .with_auth_token(Some("t1".into()))
                .serve_with_shutdown(async move { let _ = rx.await; })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let s = reqwest::get(format!("http://127.0.0.1:{port}/__asw/status"))
            .await.unwrap().status();
        assert_eq!(s, 401);
        let s2 = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/__asw/status"))
            .header("Authorization", "Bearer t1")
            .send().await.unwrap().status();
        assert_eq!(s2, 200);
        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p agent-switch-core --lib status_endpoint`
Expected: 404（路由不存在）

- [ ] **Step 3: 实现 status.rs**

```rust
use axum::{extract::State, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use super::{Circuit, ProxyService};

#[derive(Serialize, Deserialize)]
pub struct CircuitDto {
    pub provider_id: String,
    pub fails: u32,
    pub cooldown_remaining_secs: Option<u64>,
}

pub fn circuit_to_dto(provider_id: &str, c: &Circuit) -> CircuitDto {
    let cooldown_remaining_secs = c.cooldown_until.and_then(|t| {
        let now = Instant::now();
        if t > now { Some(t.saturating_duration_since(now).as_secs()) } else { None }
    });
    CircuitDto { provider_id: provider_id.to_string(), fails: c.fails, cooldown_remaining_secs }
}

#[derive(Serialize, Deserialize)]
pub struct StatusDto {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub auth_enabled: bool,
    pub routes: Vec<String>,
    pub model_override: Option<String>,
    pub target_protocol: String,
    pub circuits: Vec<CircuitDto>,
}

pub async fn handle(State(svc): State<Arc<ProxyService>>) -> impl IntoResponse {
    let (routes, model_override, target_protocol) = svc
        .core
        .db()
        .get_routes()
        .ok()
        .flatten()
        .unwrap_or((vec![], None, "chat".into()));
    let circuits = svc
        .circuits
        .lock()
        .unwrap()
        .iter()
        .map(|(id, c)| circuit_to_dto(id, c))
        .collect::<Vec<_>>();
    Json(StatusDto {
        running: true,
        host: svc.host.clone(),
        port: svc.port,
        auth_enabled: svc.auth_token.is_some(),
        routes,
        model_override,
        target_protocol,
        circuits,
    })
}
```

- [ ] **Step 4: build_router 加路由（mod.rs）**

`mod.rs` 加 `pub mod status;`（必须 `pub`，GUI 跨 crate 引用 `CircuitDto`/`StatusDto`）；`build_router` 的 protected 子路由加：
```rust
.route("/__asw/status", get(status::handle))
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p agent-switch-core --lib status_endpoint`
Expected: 绿

- [ ] **Step 6: GUI dto re-export**

`crates/gui/src-tauri/src/dto.rs`：删除本地 `CircuitDto`/`circuit_dto`，改为 `pub use agent_switch_core::proxy::status::{CircuitDto, circuit_to_dto as circuit_dto};`（保持 GUI 调用名不变）。`ProxyStatus` 的 `circuits: Vec<CircuitDto>` 类型随之来自 core。

- [ ] **Step 7: clippy/fmt + 提交**

```bash
cargo clippy --all-targets -- -D warnings && cargo fmt --all
git add -A && git commit -m "feat(core): GET /__asw/status endpoint + shared CircuitDto"
```

---

## Task 4: CLI serve install/uninstall/status/stop

**Files:**
- Modify: `crates/cli/src/main.rs`（Cmd::Serve 子命令化）
- Create: `crates/cli/src/daemon.rs`（plist/daemon.json/launchctl）

- [ ] **Step 1: 写失败测试**

`crates/cli/src/daemon.rs` 末尾测试模块（用 tempdir HOME，不实际 launchctl load）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn setup_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn install_writes_plist_and_daemon_json() {
        let dir = setup_home();
        let home = dir.path();
        let plist = plist_path(home);
        let cfg = daemon::daemon_json_path(&data_dir(home));
        assert!(!plist.exists());
        install(home, "127.0.0.1", 24860, Some("tok".into()), /*load=*/ false).unwrap();
        assert!(plist.exists());
        assert!(cfg.exists());
        let plist_txt = std::fs::read_to_string(&plist).unwrap();
        assert!(plist_txt.contains("ai.agent-switch.serve"));
        assert!(plist_txt.contains("--foreground"));
        assert!(plist_txt.contains("24860"));
        assert!(plist_txt.contains("tok"));
        let cfg_v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cfg_v["port"], 24860);
        assert_eq!(cfg_v["auth_token"], "tok");
    }

    #[test]
    fn uninstall_removes_files() {
        let dir = setup_home();
        let home = dir.path();
        install(home, "127.0.0.1", 24860, None, false).unwrap();
        uninstall(home, /*unload=*/ false).unwrap();
        assert!(!plist_path(home).exists());
        assert!(!daemon::daemon_json_path(&data_dir(home)).exists());
    }

    #[test]
    fn uninstall_idempotent_when_not_installed() {
        let dir = setup_home();
        uninstall(dir.path(), false).unwrap();
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p asw --lib daemon`
Expected: 编译失败（`daemon` 模块/函数不存在）

- [ ] **Step 3a: core/src/daemon.rs（DaemonConfig 共享类型）**

`DaemonConfig` 放 core，CLI 与 GUI 共用（消除 Task 4/5 矛盾）：

```rust
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
    pub auth_token: Option<String>,
}

pub fn daemon_json_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("daemon.json")
}

pub fn read(data_dir: &Path) -> Option<DaemonConfig> {
    serde_json::from_str(&std::fs::read_to_string(daemon_json_path(data_dir)).ok()?).ok()
}

pub fn write(data_dir: &Path, cfg: &DaemonConfig) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(daemon_json_path(data_dir), serde_json::to_string(cfg).unwrap())?;
    Ok(())
}
```

`crates/core/src/lib.rs` 加 `pub mod daemon;`。

- [ ] **Step 3b: CLI daemon.rs（plist + launchctl，复用 core::daemon）**

```rust
use agent_switch_core::daemon::{self, DaemonConfig};
use agent_switch_core::{CoreError, Result};
use std::path::{Path, PathBuf};

const LABEL: &str = "ai.agent-switch.serve";

fn launch_agents_dir(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
}
pub fn plist_path(home: &Path) -> PathBuf {
    launch_agents_dir(home).join(format!("{LABEL}.plist"))
}
pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".config/agent-switch")
}

pub fn install(
    home: &Path,
    host: &str,
    port: u16,
    auth_token: Option<String>,
    load: bool,
) -> Result<()> {
    std::fs::create_dir_all(launch_agents_dir(home))?;
    let exe = std::env::current_exe()
        .map_err(|e| CoreError::Proxy(format!("current_exe: {e}")))?
        .display().to_string();
    let mut args = format!(
        "<plist version=\"1.0\"><dict>\
         <key>Label</key><string>{LABEL}</string>\
         <key>ProgramArguments</key><array>\
         <string>{exe}</string><string>serve</string><string>--foreground</string>\
         <string>--host</string><string>{host}</string>\
         <string>--port</string><string>{port}</string>"
    );
    if let Some(t) = &auth_token {
        args.push_str(&format!("<string>--auth-token</string><string>{t}</string>"));
    }
    let dd = data_dir(home);
    std::fs::create_dir_all(&dd)?;
    let log = dd.join("serve.log").display().to_string();
    args.push_str(&format!(
        "</array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\
         <key>StandardOutPath</key><string>{log}</string>\
         <key>StandardErrorPath</key><string>{log}</string></dict></plist>"
    ));
    std::fs::write(plist_path(home), args)?;
    let cfg = DaemonConfig { host: host.to_string(), port, auth_token };
    daemon::write(&dd, &cfg)?;
    if load {
        let p = plist_path(home);
        let _ = std::process::Command::new("launchctl").args(["unload", &p.display().to_string()]).output();
        std::process::Command::new("launchctl").args(["load", &p.display().to_string()])
            .status().map_err(|e| CoreError::Proxy(format!("launchctl load: {e}")))?;
    }
    Ok(())
}

pub fn uninstall(home: &Path, unload: bool) -> Result<()> {
    let p = plist_path(home);
    if unload && p.exists() {
        let _ = std::process::Command::new("launchctl").args(["unload", &p.display().to_string()]).status();
    }
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(daemon::daemon_json_path(&data_dir(home)));
    Ok(())
}
```

- [ ] **Step 4: CLI Serve 子命令化（main.rs）**

```rust
#[derive(Subcommand)]
enum ServeCmd {
    /// 前台运行（plist 调用 / 默认）
    Foreground {
        #[arg(long, default_value = "24860")] port: u16,
        #[arg(long, default_value = "127.0.0.1")] host: String,
        #[arg(long)] auth_token: Option<String>,
    },
    /// 安装为 launchd 常驻 daemon
    Install {
        #[arg(long, default_value = "24860")] port: u16,
        #[arg(long, default_value = "127.0.0.1")] host: String,
        #[arg(long)] auth_token: Option<String>,
    },
    /// 卸载 daemon
    Uninstall,
    /// 停止 daemon（uninstall 别名）
    Stop,
    /// 查询状态
    Status,
}
```

`Cmd::Serve` 改为：
```rust
Serve {
    #[command(subcommand)]
    cmd: Option<ServeCmd>,
},
```

`run()` 处理：
```rust
Cmd::Serve { cmd } => match cmd.unwrap_or(ServeCmd::Foreground { port: 24860, host: "127.0.0.1".into(), auth_token: None }) {
    ServeCmd::Foreground { port, host, auth_token } => {
        let core = build_core()?;
        let rt = tokio::runtime::Runtime::new().map_err(|e| CoreError::Proxy(format!("rt: {e}")))?;
        rt.block_on(async {
            ProxyService::new(core).with_host_port(&host, port).with_auth_token(auth_token).serve().await
        })
    }
    ServeCmd::Install { port, host, auth_token } => {
        let home = std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
        daemon::install(std::path::Path::new(&home), &host, port, auth_token, /*load=*/ true)?;
        println!("installed daemon (label {LABEL}); logs: ~/.config/agent-switch/serve.log");
        Ok(())
    }
    ServeCmd::Uninstall | ServeCmd::Stop => {
        let home = std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
        daemon::uninstall(std::path::Path::new(&home), /*unload=*/ true)?;
        println!("uninstalled daemon");
        Ok(())
    }
    ServeCmd::Status => {
        let home = std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
        let home = std::path::Path::new(&home);
        let dd = daemon::data_dir(home);
        match agent_switch_core::daemon::read(&dd) {
            None => { println!("daemon: not installed"); Ok(()) }
            Some(cfg) => {
                let url = format!("http://{}:{}/__asw/status", cfg.host, cfg.port);
                let rt = tokio::runtime::Runtime::new().map_err(|e| CoreError::Proxy(format!("rt: {e}")))?;
                let res: Result<serde_json::Value, ()> = rt.block_on(async {
                    let client = reqwest::Client::new();
                    let mut req = client.get(&url);
                    if let Some(t) = &cfg.auth_token {
                        req = req.header("Authorization", format!("Bearer {t}"));
                    }
                    match req.send().await {
                        Ok(r) if r.status().is_success() => Ok(r.json().await.unwrap_or_default()),
                        Ok(r) => { println!("daemon: HTTP {}", r.status()); Err(()) }
                        Err(_) => { println!("daemon: not responding (check ~/.config/agent-switch/serve.log)"); Err(()) }
                    }
                });
                if let Ok(v) = res {
                    println!("daemon: running :{} auth={}", cfg.port, cfg.auth_token.is_some());
                    println!("routes: {:?}", v["routes"]);
                }
                Ok(())
            }
        }
    }
},
```

`mod daemon;` 声明；`daemon::data_dir` 在 CLI daemon.rs 设为 `pub`。CLI `Cargo.toml` 加 `reqwest = { workspace = true }`（workspace reqwest 无 blocking feature，用 async + tokio Runtime，如上）。`plist_path`/`data_dir` 用 `pub` 暴露。

注意：CLI 集成测试用 `HOME=tempdir`，plist/daemon.json 落在 tempdir 下，测试 `install(...,load=false)` 不调 launchctl。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p asw`
Expected: 绿（含 daemon 模块 3 测试 + 原 13）

- [ ] **Step 6: clippy/fmt + 提交**

```bash
cargo clippy --all-targets -- -D warnings && cargo fmt --all
git add -A && git commit -m "feat(cli): asw serve install/uninstall/status/stop (launchd daemon)"
```

---

## Task 5: GUI daemon_ctl + Proxy 命令重接

**Files:**
- Create: `crates/gui/src-tauri/src/daemon_ctl.rs`
- Delete/Replace: `crates/gui/src-tauri/src/proxy_ctl.rs`（移除内嵌 spawn）
- Modify: `crates/gui/src-tauri/src/commands.rs`、`state.rs`、`lib.rs`

- [ ] **Step 1: 写失败测试**

`daemon_ctl.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agent_switch_core::store::secrets::MockStore;
    use agent_switch_core::{proxy::ProxyService, Core};
    use std::sync::Arc;

    #[tokio::test]
    async fn status_queries_running_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::with_paths(
            &dir.path().join("home"), &dir.path().join("data"),
            Arc::new(MockStore::default()),
        ).unwrap();
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let h = tokio::spawn(async move {
            ProxyService::new(core).with_host_port("127.0.0.1", port)
                .serve_with_shutdown(async move { let _ = rx.await; }).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let s = status("127.0.0.1", port, None).await.unwrap();
        assert!(s.running);
        assert_eq!(s.port, Some(port));
        tx.send(()).unwrap();
        h.await.unwrap().unwrap();
    }

    #[test]
    fn install_args_build_correctly() {
        // 校验构造的 asw serve install 参数（不实际执行）
        let args = install_args("127.0.0.1", 24860, Some("t".into()));
        assert_eq!(args, vec!["serve", "install", "--host", "127.0.0.1", "--port", "24860", "--auth-token", "t"]);
    }
}
```

- [ ] **Step 2: 实现 daemon_ctl.rs**

```rust
use crate::dto::ProxyStatus;
use agent_switch_core::proxy::status::{CircuitDto, StatusDto};
use reqwest::Client;

pub async fn status(host: &str, port: u16, auth_token: Option<&str>) -> Result<ProxyStatus, String> {
    let url = format!("http://{host}:{port}/__asw/status");
    let client = Client::new();
    let mut req = client.get(&url);
    if let Some(t) = auth_token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(ProxyStatus { running: false, port: None, host: None, auth_enabled: false, circuits: vec![] });
    }
    let s: StatusDto = resp.json().await.map_err(|e| e.to_string())?;
    Ok(ProxyStatus { running: s.running, port: Some(s.port), host: Some(s.host), auth_enabled: s.auth_enabled, circuits: s.circuits })
}

pub fn install_args(host: &str, port: u16, auth_token: Option<String>) -> Vec<String> {
    let mut v = vec!["serve".into(), "install".into(), "--host".into(), host.into(), "--port".into(), port.to_string()];
    if let Some(t) = auth_token { v.push("--auth-token".into()); v.push(t); }
    v
}

pub fn uninstall_args() -> Vec<String> {
    vec!["serve".into(), "uninstall".into()]
}

/// 用 current_exe 的 asw 二进制执行子命令。
fn run_asw(args: Vec<String>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = std::process::Command::new(exe).args(&args).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("asw {:?} exited {status}", args)) }
}

pub fn install(host: String, port: u16, auth_token: Option<String>) -> Result<(), String> {
    run_asw(install_args(&host, port, auth_token))
}
pub fn uninstall() -> Result<(), String> {
    run_asw(uninstall_args())
}
```

`ProxyStatus` 在 `dto.rs` 改为复用 core 的 `CircuitDto`（Task 3 已 re-export）。`ProxyStatus` 字段保持（running/port/host/auth_enabled/circuits）。

- [ ] **Step 3: commands.rs 重接**

`proxy_start`/`proxy_stop`/`proxy_status` 改为：
```rust
#[tauri::command]
pub async fn proxy_start(state: State<'_, AppState>, host: String, port: u16, auth_token: Option<String>) -> CmdResult<ProxyStatus> {
    daemon_ctl::install(host, port, auth_token).map_err(String::from)?;
    // install 后稍候查询
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let cfg = daemon_ctl::DaemonConfig::read(...); // 或直接用传入 host/port/token
    Ok(daemon_ctl::status(&host, port, auth_token.as_deref()).await.unwrap_or(ProxyStatus { running:false,port:None,host:None,auth_enabled:false,circuits:vec![] }))
}
#[tauri::command]
pub async fn proxy_stop() -> CmdResult<ProxyStatus> {
    daemon_ctl::uninstall().map_err(String::from)?;
    Ok(ProxyStatus { running:false,port:None,host:None,auth_enabled:false,circuits:vec![] })
}
#[tauri::command]
pub async fn proxy_status(state: State<'_, AppState>) -> CmdResult<ProxyStatus> {
    // 读 daemon.json (需 home/data_dir) → 查询；未安装返回 stopped
    ...
}
```

`proxy_status` 需读 daemon.json：复用 `agent_switch_core::daemon::read(&core.data_dir)`（Core 需暴露 `data_dir` 访问器，或 state 持有 data_dir 路径）。`DaemonConfig` 已在 core（Task 4 Step 3a），GUI 直接 `agent_switch_core::daemon::read`/`DaemonConfig`，无需重复定义。

- [ ] **Step 4: 删除 proxy_ctl.rs，state.rs 改用 daemon_ctl**

`state.rs` 的 `proxy: Arc<ProxyCtl>` 字段移除（daemon_ctl 为无状态自由函数）；`AppState` 只留 `core`。`lib.rs` 的 `mod proxy_ctl;` → `mod daemon_ctl;`。

core 增 `Core::data_dir(&self) -> &Path` 访问器（service.rs），供 GUI 读 daemon.json 路径。

- [ ] **Step 5: 跑测试 + 构建**

Run: `cargo test -p asw-gui && cargo clippy --all-targets -- -D warnings && cargo fmt --all`
Expected: 绿

- [ ] **Step 6: 提交**

```bash
git add -A && git commit -m "feat(gui): daemon_ctl replaces embedded proxy_ctl"
```

---

## Task 6: GUI 托盘 + 关窗到托盘 + autostart

**Files:**
- Modify: `crates/gui/src-tauri/Cargo.toml`（加 tauri-plugin-autostart + reqwest）
- Create: `crates/gui/src-tauri/src/tray.rs`
- Modify: `crates/gui/src-tauri/src/lib.rs`（托盘 + 关窗事件 + autostart 插件）

- [ ] **Step 1: 加依赖**

`Cargo.toml` `[dependencies]` 加 `tauri-plugin-autostart = "2"`。

- [ ] **Step 2: tray.rs 菜单构建**

```rust
use tauri::menu::{Menu, MenuItem, Submenu, PredefinedMenuItem, MenuEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri::tray::TrayIconBuilder;

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "代理: 未运行", false, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let claude_sub = Submenu::with_items(app, "切换 Claude Code", true, &[])?; // 动态填充见 refresh
    let codex_sub = Submenu::with_items(app, "切换 Codex", true, &[])?;
    let opencode_sub = Submenu::with_items(app, "切换 OpenCode", true, &[])?;
    let quit = MenuItem::with_id(app, "quit", "退出 GUI", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &status, &start, &stop, &sep, &claude_sub, &codex_sub, &opencode_sub, &sep, &quit])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .on_menu_event(on_menu_event)
        .build(app)?;
    Ok(())
}

fn on_menu_event(app: &AppHandle, e: MenuEvent) {
    match e.id().as_ref() {
        "show" => { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.set_focus(); } }
        "start" => { /* emit to frontend or call daemon_ctl::install via spawn */ }
        "stop" => { /* daemon_ctl::uninstall */ }
        "quit" => { app.exit(0); }
        id if id.starts_with("use:") => { /* "use:<tool>:<id>" → invoke use_provider command */ }
        _ => {}
    }
}
```

provider 列表动态填充：`refresh(app)` 用 `state.core.list(None)` 重建三个 Submenu 的 items（id=`use:<tool>:<id>`，✓ 标 active）。定时：`tauri::async_runtime::spawn` 每 5s 调 `refresh`（查 daemon_ctl::status 更新 status 文本 + 重建 provider 子菜单）。

- [ ] **Step 3: lib.rs 关窗到托盘 + autostart + tray init**

```rust
.use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
// setup:
let _ = app.autolaunch().enable(); // 或按设置开关
tray::build(app.handle())?;
// on_window_event:
.on_window_event(|window, event| {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let _ = window.hide();
        api.prevent_close();
    }
})
```
注册 `tauri_plugin_autostart::init()` 插件。

- [ ] **Step 4: 构建 + 手动验证**

Run: `cargo build -p asw-gui && cargo clippy --all-targets -- -D warnings && cargo fmt --all`
手动（用户）：`cd crates/gui && npx tauri dev` → 关窗隐藏到托盘 → 托盘菜单可见 → 启动/停止代理 → 快速切换。

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(gui): system tray (close-to-tray + quick-switch + autostart)"
```

---

## Task 7: 冒烟 + 文档

**Files:** `README.md`

- [ ] **Step 1: 全量测试 + lint**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check && (cd crates/gui && npx tsc --noEmit && npm test)`
Expected: 全绿

- [ ] **Step 2: 冒烟（用户手动，macOS）**

```
asw serve install --port 24860
asw serve status                         # running :24860
curl -s http://127.0.0.1:24860/__asw/status   # JSON
curl -X POST http://127.0.0.1:24860/v1/chat/completions -H "Authorization: Bearer any" -d '{"model":"x","messages":[]}'
cd crates/gui && npx tauri dev            # 托盘控制 daemon、快速切换
asw serve uninstall
```

- [ ] **Step 3: README 追加 daemon 段**

```markdown
## daemon（macOS）

\`\`\`bash
asw serve install [--port 24860] [--host 127.0.0.1] [--auth-token T]  # 安装 launchd 常驻 + 自启
asw serve status                                                       # 查询运行/路由/熔断
asw serve uninstall                                                    # 卸载
\`\`\`
日志：`~/.config/agent-switch/serve.log`。GUI 托盘可启停同一 daemon。
```

- [ ] **Step 4: 提交**

```bash
git add -A && git commit -m "docs: daemon + tray usage"
```

---

## 完成标准

- 全 workspace `cargo test` 绿（core + cli + asw-gui）；`cargo clippy --all-targets -- -D warnings` 净；`cargo fmt --check` 净；前端 `tsc` + vitest 绿。
- macOS：`asw serve install` 常驻 + 开机自启 + 崩溃重启；`/__asw/status` 可查；GUI 托盘启停 daemon + 快速切换 + 关窗到托盘。
- Linux/Windows：前台 `asw serve` + GUI 托盘关窗可用（install 报错提示 macOS 限定）。
