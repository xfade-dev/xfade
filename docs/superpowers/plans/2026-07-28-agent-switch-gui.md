# Agent Switch Tauri GUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 `asw` 交付 Tauri v2 桌面 GUI（标准窗口），以库形式依赖 `agent-switch-core`，功能对齐全部 CLI（provider 管理 + 内嵌代理启停/路由 + stats/logs），React+TS 前端。

**Architecture:** 新 crate `crates/gui`（前端在 `crates/gui/src`，Rust 后端在 `crates/gui/src-tauri`）。GUI 通过 Tauri commands 直调 `Core`（`spawn_blocking` 包同步调用），代理内嵌同进程：`ProxyService::new(core.clone()).serve_with_shutdown(host, port, oneshot)`，启停由 `proxy_ctl` 状态机管理；运行中改路由经 `db().set_routes()` 立即生效。core 做三处向后兼容增量（serde derive、`Core: Clone`、`serve_with_shutdown`）。

**Tech Stack:** Rust（tauri 2、agent-switch-core、tokio、serde、axum/reqwest 经 core）、React 18 + TypeScript + Vite、Tailwind CSS、vitest。

**Spec:** `docs/superpowers/specs/2026-07-27-agent-switch-gui-design.md`

---

## 文件结构总览

**core 增量（修改）：**
- `crates/core/src/store/db.rs` — `RequestLog`/`StatsRow`/`StatsGroupBy` 加 derive
- `crates/core/src/presets.rs` — `Preset` 加 derive
- `crates/core/src/service.rs` — `Core` 字段 `Box<dyn SecretStore>`→`Arc<dyn SecretStore>`，derive `Clone`，`with_paths`/`for_current_user` 签名
- `crates/core/src/proxy/mod.rs` — 新增 `serve_with_shutdown`，`serve` 委派之
- `crates/core/Cargo.toml` — tokio features 显式
- `crates/core/src/store/secrets.rs` — 无代码改（trait 仍 `Send + Sync`）
- 调用点 `Box::new(...)`→`Arc::new(...)`：`crates/core/src/service.rs:33`、`crates/core/src/service.rs:196`（test）、`crates/cli/src/main.rs:164-168`、`crates/core/src/proxy/testsupport.rs:266`

**GUI 新建：**
- `crates/gui/src-tauri/Cargo.toml`、`build.rs`、`tauri.conf.json`、`capabilities/default.json`、`src/main.rs`、`src/commands.rs`、`src/dto.rs`、`src/proxy_ctl.rs`、`src/state.rs`
- `crates/gui/package.json`、`vite.config.ts`、`tsconfig.json`、`index.html`、`src/main.tsx`、`src/App.tsx`、`src/api.ts`、`src/types.ts`、`src/components/*`、`src/pages/*`、`src/index.css`
- workspace 根 `Cargo.toml`：`members` 加 `crates/gui/src-tauri`，加 `tauri`/`tauri-build` 到 `[workspace.dependencies]`

---

## Task 1: core serde derive（RequestLog/StatsRow/StatsGroupBy/Preset）

**Files:**
- Modify: `crates/core/src/store/db.rs:10,24,30`
- Modify: `crates/core/src/presets.rs:3`

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/store/db.rs` 的 `#[cfg(test)] mod tests` 末尾追加：

```rust
    #[test]
    fn request_log_serializes() {
        let log = RequestLog {
            ts: "2026-07-27T00:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("gpt-x".into()),
            provider_id: "yy".into(),
            status: 200,
            prompt_tokens: 1,
            completion_tokens: 2,
            duration_ms: 10,
            error: None,
        };
        let v: serde_json::Value = serde_json::to_value(&log).unwrap();
        assert_eq!(v["endpoint"], "chat");
        assert_eq!(v["status"], 200);
    }

    #[test]
    fn stats_group_by_roundtrip() {
        for b in [StatsGroupBy::Provider, StatsGroupBy::Model] {
            let s = serde_json::to_string(&b).unwrap();
            let back: StatsGroupBy = serde_json::from_str(&s).unwrap();
            assert!(matches!(back, _)) ;
        }
    }
```

在 `crates/core/src/presets.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn preset_serializes() {
        let p = presets_for(crate::models::ToolKind::Codex)[0].clone();
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert!(v["id"].is_string());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p agent-switch-core --lib request_log_serializes stats_group_by_roundtrip preset_serializes`
Expected: 编译失败（`Serialize` not implemented）

- [ ] **Step 3: 最小实现**

`crates/core/src/store/db.rs`：

```rust
#[derive(Debug, serde::Serialize)]
pub struct RequestLog {
```
```rust
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum StatsGroupBy {
    Provider,
    Model,
}
```
```rust
#[derive(Debug, serde::Serialize)]
pub struct StatsRow {
```

`crates/core/src/presets.rs`：

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct Preset {
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p agent-switch-core --lib`
Expected: 全绿（原 70 测试 + 新 3）

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/store/db.rs crates/core/src/presets.rs
git commit -m "feat(core): serde derive for RequestLog/StatsRow/StatsGroupBy/Preset"
```

---

## Task 2: Core: Clone（Arc<dyn SecretStore>）

**Files:**
- Modify: `crates/core/src/service.rs:9-34,196`
- Modify: `crates/core/src/proxy/testsupport.rs:263-266`
- Modify: `crates/cli/src/main.rs:164-168`

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/service.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn core_clone_shares_db() {
        let (_dir, core) = setup();
        core.add_provider(
            Provider::new("kimi", ToolKind::Codex, Some("https://x".into())),
            Some("k"),
        )
        .unwrap();
        let core2 = core.clone();
        // 通过 clone 写入，原实例可见
        core2.add_provider(
            Provider::new("bak", ToolKind::Codex, Some("https://y".into())),
            Some("k2"),
        )
        .unwrap();
        assert_eq!(core.list(Some(ToolKind::Codex)).unwrap().len(), 2);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p agent-switch-core --lib core_clone_shares_db`
Expected: 编译失败（`Core` 未实现 `Clone`）

- [ ] **Step 3: 实现**

`crates/core/src/service.rs`：

```rust
use std::sync::Arc;

#[derive(Clone)]
pub struct Core {
    db: Database,
    secrets: Arc<dyn SecretStore>,
    home: PathBuf,
    data_dir: PathBuf,
}

impl Core {
    pub fn with_paths(home: &Path, data_dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        Ok(Self {
            db: Database::open(&data_dir.join("agent-switch.db"))?,
            secrets,
            home: home.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn for_current_user() -> Result<Self> {
        let home =
            dirs::home_dir().ok_or_else(|| CoreError::Keyring("cannot locate home dir".into()))?;
        let data = home.join(".config").join("agent-switch");
        Self::with_paths(&home, &data, Arc::new(KeyringStore::new()))
    }
```

`secrets()` 方法返回类型不变（`&dyn SecretStore`，`&*self.secrets` 仍有效）。

更新调用点 `Box::new(` → `Arc::new(`：
- `crates/core/src/service.rs` test `setup()`：`Arc::new(MockStore::default())`
- `crates/core/src/proxy/testsupport.rs:266`：`Arc::new(crate::store::secrets::MockStore::default())`
- `crates/cli/src/main.rs` `build_core()`：
  ```rust
  let secrets: Arc<dyn SecretStore> = if use_mock {
      Arc::new(FileMockStore::new(data.join("mock-secrets.json")))
  } else {
      Arc::new(KeyringStore::new())
  };
  ```
- 顶部 `use`：cli/main.rs 已 `use std::sync::Arc;`? 否。在 cli/main.rs 顶部加 `use std::sync::Arc;`（若未引入）。testsupport.rs 顶部已有 `use std::sync::{Arc, Mutex};`。service.rs 新增 `use std::sync::Arc;`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: core + cli 全绿（83 + 新增）

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "feat(core): Core: Clone via Arc<dyn SecretStore>"
```

---

## Task 3: serve_with_shutdown + tokio features

**Files:**
- Modify: `crates/core/src/proxy/mod.rs:86-95`
- Modify: `crates/core/Cargo.toml:14`

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/proxy/mod.rs` 的 `mod tests` 追加：

```rust
    #[tokio::test]
    async fn serve_with_shutdown_stops_and_releases_port() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        // 取一个空闲端口
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let core2 = core.clone();
        let handle = tokio::spawn(async move {
            ProxyService::new(core2)
                .serve_with_shutdown("127.0.0.1", port, rx)
                .await
        });
        // 等监听就绪
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        tx.send(()).unwrap();
        handle.await.unwrap().unwrap();

        // 端口已释放：可重新绑定
        let rebind = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await;
        assert!(rebind.is_ok(), "port should be released after graceful shutdown");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p agent-switch-core --lib serve_with_shutdown_stops_and_releases_port`
Expected: 编译失败（`serve_with_shutdown` 不存在）

- [ ] **Step 3: 实现**

`crates/core/src/proxy/mod.rs`，替换 `serve` 方法（:86-95）：

```rust
    /// 启动代理；`shutdown` future 完成时优雅关停。GUI 用 oneshot receiver。
    pub async fn serve_with_shutdown(
        self,
        host: &str,
        port: u16,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<()> {
        let app = self.build_router();
        let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
            .await
            .map_err(|e| crate::error::CoreError::Proxy(format!("bind {host}:{port}: {e}")))?;
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| crate::error::CoreError::Proxy(format!("serve: {e}")))?;
        Ok(())
    }

    /// CLI 用：阻塞运行直到中断（永不触发优雅关停信号）。
    pub async fn serve(self, host: &str, port: u16) -> Result<()> {
        self.serve_with_shutdown(host, port, std::future::pending::<()>())
            .await
    }
```

`crates/core/Cargo.toml`：

```toml
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "time", "sync"] }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全绿（含新测试，原 CLI serve 行为不变）

- [ ] **Step 5: clippy/fmt**

Run: `cargo clippy --all-targets -- -D warnings && cargo fmt --all`
Expected: 干净

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "feat(core): serve_with_shutdown + explicit tokio features"
```

---

## Task 4: G1 验收

- [ ] **Step 1: 全量测试 + lint**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: 全绿，clippy/fmt 净（83 + G1 新增 ≈ 86）

- [ ] **Step 2: 提交（若有残留）** — 无则跳过

---

## Task 5: Tauri 脚手架（空壳窗口）

**Files:**
- Create: `crates/gui/package.json`, `crates/gui/vite.config.ts`, `crates/gui/tsconfig.json`, `crates/gui/index.html`, `crates/gui/src/main.tsx`, `crates/gui/src/App.tsx`, `crates/gui/src/index.css`
- Create (via `cargo tauri init`): `crates/gui/src-tauri/*`
- Modify: `Cargo.toml`（workspace members + deps）, `crates/gui/src-tauri/Cargo.toml`, `crates/gui/src-tauri/tauri.conf.json`, `crates/gui/src-tauri/src/main.rs`

- [ ] **Step 1: 前端骨架**

`crates/gui/package.json`：

```json
{
  "name": "asw-gui",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "preview": "vite preview",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "^18.3.1",
    "react-dom": "^18.3.1",
    "@tauri-apps/api": "^2.0.0"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2.0.0",
    "@types/react": "^18.3.0",
    "@types/react-dom": "^18.3.0",
    "@vitejs/plugin-react": "^4.3.0",
    "typescript": "^5.5.0",
    "vite": "^5.4.0",
    "vitest": "^2.0.0",
    "tailwindcss": "^3.4.0",
    "postcss": "^8.4.0",
    "autoprefixer": "^10.4.0"
  }
}
```

`crates/gui/vite.config.ts`：

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "esnext", outDir: "dist" },
  test: { environment: "jsdom" },
});
```

`crates/gui/tsconfig.json`：

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["src"]
}
```

`crates/gui/index.html`：

```html
<!doctype html>
<html lang="zh">
  <head><meta charset="UTF-8" /><title>Agent Switch</title></head>
  <body><div id="root"></div><script type="module" src="/src/main.tsx"></script></body>
</html>
```

`crates/gui/src/main.tsx`：

```tsx
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><App /></React.StrictMode>
);
```

`crates/gui/src/App.tsx`：

```tsx
export default function App() {
  return <div className="p-8 text-lg">Agent Switch GUI</div>;
}
```

`crates/gui/src/index.css`：

```css
@tailwind base;
@tailwind components;
@tailwind utilities;
```

`crates/gui/tailwind.config.js`：

```js
export default { content: ["./index.html", "./src/**/*.{ts,tsx}"] };
```

`crates/gui/postcss.config.js`：

```js
export default { plugins: { tailwindcss: {}, autoprefixer: {} } };
```

- [ ] **Step 2: 安装前端依赖**

Run: `cd crates/gui && npm install`
Expected: node_modules 就绪

- [ ] **Step 3: workspace 注册 + tauri init**

根 `Cargo.toml`：

```toml
members = ["crates/core", "crates/cli", "crates/gui/src-tauri"]
```
并在 `[workspace.dependencies]` 追加：

```toml
tauri = { version = "2", features = [] }
tauri-build = { version = "2", features = [] }
```

Run: `cd crates/gui && cargo tauri init --ci --app-name asw-gui --window-title "Agent Switch" --frontend-dir ../src --frontend-dist ../dist --before-dev-command "npm run dev" --before-build-command "npm run build"`
Expected: 生成 `crates/gui/src-tauri/`（含 `Cargo.toml`、`tauri.conf.json`、`build.rs`、`src/main.rs`、`capabilities/default.json`、`icons/`）

- [ ] **Step 4: 接入 core 依赖 + 最小 main.rs**

编辑 `crates/gui/src-tauri/Cargo.toml`，在 `[dependencies]` 加：

```toml
agent-switch-core = { path = "../../core" }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "sync", "net", "time"] }
tauri = { workspace = true, features = [] }
```

`crates/gui/src-tauri/src/main.rs`（覆盖生成内容）：

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 5: 启动验证**

Run: `cd crates/gui && cargo tauri dev`
Expected: 编译通过，弹出标题 "Agent Switch" 的窗口，显示 "Agent Switch GUI"。Ctrl-C 退出。

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "feat(gui): tauri v2 scaffold with react+vite+tailwind"
```

---

## Task 6: dto.rs + proxy_ctl.rs（状态机）

**Files:**
- Create: `crates/gui/src-tauri/src/dto.rs`
- Create: `crates/gui/src-tauri/src/proxy_ctl.rs`
- Create: `crates/gui/src-tauri/src/state.rs`

- [ ] **Step 1: dto.rs**

```rust
use agent_switch_core::models::{Provider, ToolKind};
use agent_switch_core::store::db::{StatsGroupBy, StatsRow, RequestLog};
use agent_switch_core::proxy::Circuit;
use agent_switch_core::presets::Preset;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Serialize, Deserialize)]
pub struct AddProviderInput {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct UpdateProviderInput {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Serialize)]
pub struct PresetDto {
    pub id: String,
    pub label: String,
    pub base_url: Option<String>,
    pub extra: serde_json::Value,
    pub is_official: bool,
}

impl From<Preset> for PresetDto {
    fn from(p: Preset) -> Self {
        Self {
            id: p.id.to_string(),
            label: p.label.to_string(),
            base_url: p.base_url.map(String::from),
            extra: p.extra,
            is_official: p.is_official(),
        }
    }
}

#[derive(Serialize)]
pub struct CircuitDto {
    pub provider_id: String,
    pub fails: u32,
    pub cooldown_remaining_secs: Option<u64>,
}

pub fn circuit_dto(provider_id: &str, c: &Circuit) -> CircuitDto {
    let cooldown_remaining_secs = c.cooldown_until.and_then(|t| {
        let now = Instant::now();
        if t > now {
            Some(t.saturating_duration_since(now).as_secs())
        } else {
            None
        }
    });
    CircuitDto {
        provider_id: provider_id.to_string(),
        fails: c.fails,
        cooldown_remaining_secs,
    }
}

#[derive(Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub auth_enabled: bool,
    pub circuits: Vec<CircuitDto>,
}

#[derive(Serialize, Deserialize)]
pub struct RoutesDto {
    pub routes: Vec<String>,
    pub model_override: Option<String>,
    pub target_protocol: String,
}

// core 已 Serialize 的类型透传：Provider / ToolKind / StatsRow / RequestLog / StatsGroupBy
```

- [ ] **Step 2: proxy_ctl.rs**

```rust
use crate::dto::{circuit_dto, CircuitDto, ProxyStatus};
use agent_switch_core::proxy::ProxyService;
use agent_switch_core::Core;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

pub struct ProxyCtl {
    inner: Mutex<Inner>,
}

struct Inner {
    // None = 已停止
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<Result<(), agent_switch_core::CoreError>>>,
    port: Option<u16>,
    host: Option<String>,
    auth_enabled: bool,
    circuits: Option<Arc<Mutex<HashMap<String, agent_switch_core::proxy::Circuit>>>>,
}

impl Default for ProxyCtl {
    fn default() -> Self {
        Self { inner: Mutex::new(Inner { shutdown: None, handle: None, port: None, host: None, auth_enabled: false, circuits: None }) }
    }
}

impl ProxyCtl {
    pub fn status(&self) -> ProxyStatus {
        let inner = self.inner.lock().unwrap();
        let circuits = inner.circuits.as_ref().map(|arc| {
            let m = arc.lock().unwrap();
            m.iter().map(|(id, c)| circuit_dto(id, c)).collect::<Vec<_>>()
        }).unwrap_or_default();
        ProxyStatus {
            running: inner.shutdown.is_some(),
            port: inner.port,
            host: inner.host.clone(),
            auth_enabled: inner.auth_enabled,
            circuits,
        }
    }

    pub fn start(&self, core: Core, host: String, port: u16, auth_token: Option<String>) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.shutdown.is_some() {
            return Err("proxy already running".into());
        }
        let mut svc = ProxyService::new(core).with_auth_token(auth_token.clone());
        let circuits = svc.circuits.clone();
        let (tx, rx) = oneshot::channel::<()>();
        let h = host.clone();
        let host_owned = h.clone();
        let handle = tauri::async_runtime::spawn(async move {
            svc.serve_with_shutdown(&host_owned, port, rx).await
        });
        inner.shutdown = Some(tx);
        inner.handle = Some(handle);
        inner.port = Some(port);
        inner.host = Some(host);
        inner.auth_enabled = auth_token.is_some();
        inner.circuits = Some(circuits);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let (tx, handle) = {
            let mut inner = self.inner.lock().unwrap();
            match (inner.shutdown.take(), inner.handle.take()) {
                (Some(tx), Some(handle)) => (tx, handle),
                _ => return Ok(()), // 幂等：未运行直接返回
            }
        };
        let _ = tx.send(());
        let _ = handle.await;
        let mut inner = self.inner.lock().unwrap();
        inner.port = None;
        inner.host = None;
        inner.auth_enabled = false;
        inner.circuits = None;
        Ok(())
    }
}
```

- [ ] **Step 3: state.rs**

```rust
use crate::proxy_ctl::ProxyCtl;
use agent_switch_core::Core;
use std::sync::Arc;

pub struct AppState {
    pub core: Core,
    pub proxy: Arc<ProxyCtl>,
}

impl AppState {
    pub fn new(core: Core) -> Self {
        Self { core, proxy: Arc::new(ProxyCtl::default()) }
    }
}
```

- [ ] **Step 4: 写测试**

`crates/gui/src-tauri/src/proxy_ctl.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agent_switch_core::store::secrets::MockStore;
    use agent_switch_core::Core;
    use std::sync::Arc;

    fn core() -> Core {
        let dir = tempfile::tempdir().unwrap();
        Core::with_paths(
            &dir.path().join("home"),
            &dir.path().join("data"),
            Arc::new(MockStore::default()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn start_stop_idempotent() {
        let ctl = ProxyCtl::default();
        assert!(!ctl.status().running);
        // start on a free port
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        ctl.start(core(), "127.0.0.1".into(), port, None).unwrap();
        assert!(ctl.status().running);
        // double start rejected
        assert!(ctl.start(core(), "127.0.0.1".into(), port, None).is_err());
        ctl.stop().await.unwrap();
        assert!(!ctl.status().running);
        // stop again idempotent
        ctl.stop().await.unwrap();
    }
}
```

需在 `src-tauri/Cargo.toml` `[dev-dependencies]` 加 `tempfile = { workspace = true }`。

- [ ] **Step 5: 跑测试**

Run: `cargo test -p asw-gui`
Expected: 绿

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "feat(gui): dto + proxy state machine + state"
```

---

## Task 7: commands.rs — provider 管理与备份

**Files:**
- Create: `crates/gui/src-tauri/src/commands.rs`
- Modify: `crates/gui/src-tauri/src/main.rs`

- [ ] **Step 1: commands.rs（provider 部分）**

```rust
use crate::dto::{AddProviderInput, PresetDto, UpdateProviderInput};
use crate::state::AppState;
use agent_switch_core::models::{Provider, ToolKind};
use agent_switch_core::presets::presets_for;
use tauri::State;

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String { e.to_string() }

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>, tool: Option<ToolKind>) -> CmdResult<Vec<Provider>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.list(tool)).await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn add_provider(state: State<'_, AppState>, input: AddProviderInput) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut p = Provider::new(&input.id, input.tool, input.base_url);
        p.extra = input.extra;
        core.add_provider(p, input.key.as_deref())
    }).await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn update_provider(state: State<'_, AppState>, input: UpdateProviderInput) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut p = core.list(Some(input.tool))?.into_iter().find(|p| p.id == input.id)
            .ok_or_else(|| agent_switch_core::CoreError::ProviderNotFound(format!("{}/{}", input.tool, input.id)))?;
        if let Some(u) = input.base_url { p.base_url = Some(u); }
        if !input.extra.is_null() { p.extra = input.extra; }
        core.update_provider(&p, input.key.as_deref())
    }).await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn remove_provider(state: State<'_, AppState>, tool: ToolKind, id: String) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.remove(tool, &id))
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn use_provider(state: State<'_, AppState>, tool: ToolKind, id: String) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.use_provider(tool, &id))
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn current_provider(state: State<'_, AppState>, tool: ToolKind) -> CmdResult<Option<Provider>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.current(tool))
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn list_presets(tool: ToolKind) -> CmdResult<Vec<PresetDto>> {
    Ok(presets_for(tool).into_iter().map(PresetDto::from).collect())
}

#[tauri::command]
pub async fn import_provider(state: State<'_, AppState>, tool: ToolKind) -> CmdResult<Option<Provider>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.import(tool))
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn list_backups(state: State<'_, AppState>, tool: ToolKind) -> CmdResult<Vec<String>> {
    let core = state.core.clone();
    let paths = tauri::async_runtime::spawn_blocking(move || core.backups(tool))
        .await.map_err(err)?.map_err(err)?;
    Ok(paths.into_iter().map(|p| p.display().to_string()).collect())
}

#[tauri::command]
pub async fn restore_backup(state: State<'_, AppState>, tool: ToolKind, path: String) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.restore_backup(tool, std::path::Path::new(&path)))
        .await.map_err(err)?.map_err(err)
}
```

- [ ] **Step 2: 暂不注册到 main.rs（下个 Task 一起）**

- [ ] **Step 3: 测试（纯函数逻辑：preset 转换）**

在 commands.rs 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agent_switch_core::models::ToolKind;

    #[test]
    fn list_presets_converts() {
        let v = presets_for(ToolKind::Codex)
            .into_iter()
            .map(PresetDto::from)
            .collect::<Vec<_>>();
        assert!(v.iter().any(|p| p.id == "official" && p.is_official));
        assert!(v.iter().any(|p| p.id == "local-proxy" && !p.is_official));
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p asw-gui`
Expected: 绿

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "feat(gui): provider management commands"
```

---

## Task 8: commands.rs — 代理/路由/stats/logs + 接入 main.rs

**Files:**
- Modify: `crates/gui/src-tauri/src/commands.rs`
- Modify: `crates/gui/src-tauri/src/main.rs`

- [ ] **Step 1: 追加 commands**

在 `commands.rs` 追加：

```rust
use crate::dto::{ProxyStatus, RoutesDto};
use agent_switch_core::store::db::{StatsGroupBy, StatsRow, RequestLog};

#[tauri::command]
pub async fn proxy_start(state: State<'_, AppState>, host: String, port: u16, auth_token: Option<String>) -> CmdResult<ProxyStatus> {
    state.core.clone(); // ensure clone works
    state.proxy.start(state.core.clone(), host, port, auth_token)?;
    Ok(state.proxy.status())
}

#[tauri::command]
pub async fn proxy_stop(state: State<'_, AppState>) -> CmdResult<ProxyStatus> {
    state.proxy.stop().await?;
    Ok(state.proxy.status())
}

#[tauri::command]
pub async fn proxy_status(state: State<'_, AppState>) -> CmdResult<ProxyStatus> {
    Ok(state.proxy.status())
}

#[tauri::command]
pub async fn get_routes(state: State<'_, AppState>) -> CmdResult<Option<RoutesDto>> {
    let core = state.core.clone();
    let r = tauri::async_runtime::spawn_blocking(move || core.db().get_routes())
        .await.map_err(err)?.map_err(err)?;
    Ok(r.map(|(routes, model_override, target_protocol)| RoutesDto { routes, model_override, target_protocol }))
}

#[tauri::command]
pub async fn set_routes(state: State<'_, AppState>, routes: Vec<String>, model_override: Option<String>, target_protocol: String) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().set_routes(&routes, model_override.as_deref(), &target_protocol))
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn clear_routes(state: State<'_, AppState>) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().clear_routes())
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn stats(state: State<'_, AppState>, since: String, by: StatsGroupBy) -> CmdResult<Vec<StatsRow>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().stats_since(&since, by))
        .await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub async fn recent_logs(state: State<'_, AppState>, limit: usize) -> CmdResult<Vec<RequestLog>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().recent_request_logs(limit))
        .await.map_err(err)?.map_err(err)
}
```

注：`proxy_start` 中 `state.core.clone()` 两处冗余，保留一处即可（实现时清理：仅 `state.proxy.start(state.core.clone(), ...)`）。

- [ ] **Step 2: main.rs 接入**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod dto;
mod proxy_ctl;
mod state;

use agent_switch_core::store::secrets::{FileMockStore, KeyringStore};
use agent_switch_core::Core;
use state::AppState;
use std::sync::Arc;

fn build_core() -> Result<Core, String> {
    // 与 CLI 同环境契约
    let use_mock = std::env::var("ASW_MOCK_SECRETS").ok().as_deref() == Some("1");
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let data = std::env::var_os("ASW_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(&home).join(".config").join("agent-switch"));
    let secrets: Arc<dyn agent_switch_core::store::secrets::SecretStore> = if use_mock {
        Arc::new(FileMockStore::new(data.join("mock-secrets.json")))
    } else {
        Arc::new(KeyringStore::new())
    };
    Core::with_paths(std::path::Path::new(&home), &data, secrets).map_err(|e| e.to_string())
}

fn main() {
    let core = build_core().expect("failed to init core");
    tauri::Builder::default()
        .manage(AppState::new(core))
        .invoke_handler(tauri::generate_handler![
            commands::list_providers,
            commands::add_provider,
            commands::update_provider,
            commands::remove_provider,
            commands::use_provider,
            commands::current_provider,
            commands::list_presets,
            commands::import_provider,
            commands::list_backups,
            commands::restore_backup,
            commands::proxy_start,
            commands::proxy_stop,
            commands::proxy_status,
            commands::get_routes,
            commands::set_routes,
            commands::clear_routes,
            commands::stats,
            commands::recent_logs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 3: 构建验证**

Run: `cargo build -p asw-gui`
Expected: 编译通过

- [ ] **Step 4: clippy/fmt**

Run: `cargo clippy --all-targets -- -D warnings && cargo fmt --all`

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "feat(gui): proxy/routes/stats/logs commands + wire up main"
```

---

## Task 9: 前端 api.ts + 类型 + 布局

**Files:**
- Create: `crates/gui/src/types.ts`, `crates/gui/src/api.ts`, `crates/gui/src/components/Layout.tsx`, `crates/gui/src/components/Toast.tsx`
- Modify: `crates/gui/src/App.tsx`

- [ ] **Step 1: types.ts**

```ts
export type ToolKind = "claude-code" | "codex" | "open-code";

export interface Provider {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key_ref: string;
  extra: any;
  is_active: boolean;
}

export interface PresetDto {
  id: string; label: string; base_url: string | null; extra: any; is_official: boolean;
}

export interface ProxyStatus {
  running: boolean; port: number | null; host: string | null; auth_enabled: boolean;
  circuits: CircuitDto[];
}
export interface CircuitDto { provider_id: string; fails: number; cooldown_remaining_secs: number | null; }

export interface RoutesDto { routes: string[]; model_override: string | null; target_protocol: string; }

export interface StatsRow { group: string; requests: number; prompt_tokens: number; completion_tokens: number; errors: number; avg_duration_ms: number; }
export interface RequestLog { ts: string; endpoint: string; model: string | null; provider_id: string; status: number; prompt_tokens: number; completion_tokens: number; duration_ms: number; error: string | null; }

export interface AddProviderInput { id: string; tool: ToolKind; base_url: string | null; key: string | null; extra?: any; }
export interface UpdateProviderInput { id: string; tool: ToolKind; base_url: string | null; key: string | null; extra?: any; }
```

- [ ] **Step 2: api.ts**

```ts
import { invoke } from "@tauri-apps/api/core";
import type { AddProviderInput, PresetDto, Provider, ProxyStatus, RequestLog, RoutesDto, StatsRow, ToolKind, UpdateProviderInput } from "./types";

export const listProviders = (tool?: ToolKind) => invoke<Provider[]>("list_providers", { tool: tool ?? null });
export const addProvider = (input: AddProviderInput) => invoke<void>("add_provider", { input });
export const updateProvider = (input: UpdateProviderInput) => invoke<void>("update_provider", { input });
export const removeProvider = (tool: ToolKind, id: string) => invoke<void>("remove_provider", { tool, id });
export const useProvider = (tool: ToolKind, id: string) => invoke<void>("use_provider", { tool, id });
export const currentProvider = (tool: ToolKind) => invoke<Provider | null>("current_provider", { tool });
export const listPresets = (tool: ToolKind) => invoke<PresetDto[]>("list_presets", { tool });
export const importProvider = (tool: ToolKind) => invoke<Provider | null>("import_provider", { tool });
export const listBackups = (tool: ToolKind) => invoke<string[]>("list_backups", { tool });
export const restoreBackup = (tool: ToolKind, path: string) => invoke<void>("restore_backup", { tool, path });
export const proxyStart = (host: string, port: number, authToken: string | null) => invoke<ProxyStatus>("proxy_start", { host, port, authToken });
export const proxyStop = () => invoke<ProxyStatus>("proxy_stop");
export const proxyStatus = () => invoke<ProxyStatus>("proxy_status");
export const getRoutes = () => invoke<RoutesDto | null>("get_routes");
export const setRoutes = (routes: string[], modelOverride: string | null, targetProtocol: string) => invoke<void>("set_routes", { routes, modelOverride, targetProtocol });
export const clearRoutes = () => invoke<void>("clear_routes");
export const stats = (since: string, by: "Provider" | "Model") => invoke<StatsRow[]>("stats", { since, by });
export const recentLogs = (limit: number) => invoke<RequestLog[]>("recent_logs", { limit });
```

- [ ] **Step 3: Layout + Toast 组件**

`src/components/Toast.tsx`：一个简单的 `{ message, kind }` 受控提示组件，`kind: "error" | "success"`，固定右上角。

`src/components/Layout.tsx`：左侧栏四项导航（Providers / Proxy / Stats / Logs），右侧渲染 children；接受 `active` 与 `onNavigate` props。

- [ ] **Step 4: App.tsx 接入路由切换**

```tsx
import { useState } from "react";
import Layout from "./components/Layout";
import ProvidersPage from "./pages/ProvidersPage";

type Page = "providers" | "proxy" | "stats" | "logs";

export default function App() {
  const [page, setPage] = useState<Page>("providers");
  return (
    <Layout active={page} onNavigate={setPage}>
      {page === "providers" && <ProvidersPage />}
      {page === "proxy" && <div>Proxy (next task)</div>}
      {page === "stats" && <div>Stats (next task)</div>}
      {page === "logs" && <div>Logs (next task)</div>}
    </Layout>
  );
}
```

- [ ] **Step 5: vitest 冒烟**

`src/api.test.ts`：仅校验各函数为 callable（不真调 invoke，mock `@tauri-apps/api/core`）。

Run: `cd crates/gui && npm test`
Expected: 绿

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "feat(gui): frontend api layer + layout + toast"
```

---

## Task 10: Providers 页

**Files:**
- Create: `crates/gui/src/pages/ProvidersPage.tsx`, `crates/gui/src/components/ProviderForm.tsx`, `crates/gui/src/components/BackupsPanel.tsx`

- [ ] **Step 1: ProvidersPage**

功能：三工具 Tab（Claude Code / Codex / OpenCode）；当前 Tab 下 provider 列表表格（名称 / base_url / active 标记）；行操作：切换（use）、编辑（弹 ProviderForm）、删除（确认）；页级操作：新增（ProviderForm，含 preset 下拉预填）、从工具导入（import）、备份子面板（BackupsPanel）。

错误用 Toast 提示；删除/恢复二次 `confirm()`。

- [ ] **Step 2: ProviderForm**

字段：name(id)、tool（编辑时只读）、base_url（官方=空）、key（新增必填非官方，编辑可空=不改）、extra（JSON 文本框，可选）。preset 下拉选择后预填 base_url/extra。

- [ ] **Step 3: BackupsPanel**

折叠区：`list_backups(tool)` 列表 + 恢复按钮（确认后 `restore_backup`）。

- [ ] **Step 4: 手动验证**

Run: `cd crates/gui && ASW_MOCK_SECRETS=1 ASW_DATA_DIR=/tmp/asw-gui-smoke cargo tauri dev`
Expected: Providers 页能新增/切换/编辑/删除/import/查看备份（与 CLI 共享 /tmp/asw-gui-smoke 数据）。

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "feat(gui): providers page (crud/switch/import/backups)"
```

---

## Task 11: Proxy 页

**Files:**
- Create: `crates/gui/src/pages/ProxyPage.tsx`, `crates/gui/src/components/RoutesEditor.tsx`

- [ ] **Step 1: ProxyPage**

serve 控制卡：host（默认 127.0.0.1）、port（默认 24860）、auth-token（可选）输入 + 启动/停止按钮 + 状态灯（`proxy_status()` 轮询每 2s，运行中显示端口/熔断表）。启停调用 `proxy_start/stop`，失败 Toast。

- [ ] **Step 2: RoutesEditor**

候选 = 全部 provider（`list_providers()`）；多选加入路由，主→备顺序上下移动；`model_override` 输入；`target_protocol` 单选（chat/messages）；保存调 `set_routes`；清空调 `clear_routes`。加载时 `get_routes` 回填。

- [ ] **Step 3: 熔断表**

运行中时显示 `status.circuits`：provider_id / fails / 剩余冷却秒（null 显示 "—"）。

- [ ] **Step 4: 手动验证**

Run: `cargo tauri dev`（同上 env）
Expected: 启动代理 → curl /health 200 → 改路由立即生效（用 CLI `asw proxy status` 对照）→ 停止后端口释放。

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "feat(gui): proxy page (serve control + routes editor + circuits)"
```

---

## Task 12: Stats 页

**Files:**
- Create: `crates/gui/src/pages/StatsPage.tsx`

- [ ] **Step 1: 实现**

时间范围（24h/7d/30d）+ 分组（provider/model）切换；前端把 `<N>d` 转 RFC3339（`new Date(Date.now()-N*864e5).toISOString()`）调 `stats`；表格展示 group/requests/tokens/errors/avg_duration_ms。空数据显示提示。

- [ ] **Step 2: 手动验证**

产生若干请求日志后查看聚合（可用 v0.3 测试的 MockUpstream 思路，或直接看历史数据）。

- [ ] **Step 3: 提交**

```bash
git add -A
git commit -m "feat(gui): stats page"
```

---

## Task 13: Logs 页 + 整体冒烟

**Files:**
- Create: `crates/gui/src/pages/LogsPage.tsx`
- Modify: `crates/gui/src/App.tsx`（接通三页）
- Modify: `README.md`

- [ ] **Step 1: LogsPage**

`recent_logs(200)` 表格：ts/endpoint/model/provider/status/tokens/duration/error；非 2xx 行高亮（`status>=400` 红底）；手动刷新按钮。

- [ ] **Step 2: 接通 App.tsx**

替换 Task 9 中的占位 div 为 StatsPage / LogsPage / ProxyPage。

- [ ] **Step 3: 整体冒烟**

Run: `cargo tauri dev`
验收清单：
- [ ] Providers：三工具切换/import/备份可见
- [ ] Proxy：启停 + 路由热改 + 熔断展示
- [ ] Stats：聚合展示
- [ ] Logs：最近日志展示
- [ ] `cargo test`（全 workspace）绿；`cargo clippy --all-targets -- -D warnings` 净

- [ ] **Step 4: README 更新**

在 README.md 追加 "## GUI" 段：构建 `cd crates/gui && cargo tauri dev`（dev）/ `cargo tauri build`（产物 `.app`）；环境契约与 CLI 一致。

- [ ] **Step 5: 提交**

```bash
git add -A
git commit -m "feat(gui): logs page + smoke + docs"
```

---

## 完成标准

- 全 workspace `cargo test` 绿（core 86 + gui N）。
- `cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` 净。
- `cargo tauri dev` 四页功能可用，代理启停与路由热改即时生效。
- 与 CLI 共享数据目录/钥匙串，可同时使用。
