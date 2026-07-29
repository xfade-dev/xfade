# Changelog

本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## v0.6.0 — 2026-07-29

### 技术债
- **fix(core)**: codex apply 移除官方 OAuth `tokens` + 设 `preferred_auth_method=apikey`，防 Authorization 被劫持。
- **fix(core)**: stats errors 计入 `status=0`（上游连接失败）。
- **feat(core)**: codex apply 后扫描全文件废弃 `wire_api="chat"` 并警告。

### 打包发布
- **chore**: workspace `[workspace.package]` metadata（v0.6.0，license MIT OR Apache-2.0）。
- **ci**: CI workflow（fmt/clippy/test + 前端 tsc/vitest）；release workflow（tag 触发多平台 CLI 二进制 + macOS GUI .dmg + checksums + GitHub Release）。
- **feat(gui)**: `asw` 作为 Tauri sidecar 打入 .app，GUI 经 `tauri-plugin-shell` 自装 daemon（解决 Finder PATH 问题）。
- **feat**: Homebrew formula（独立 tap `homebrew-agent-switch`）。
- `core::daemon::install` 幂等（unload-then-load）。
- `RELEASE.md` 打包/发布流程文档。

### 文档
- README 重构；新增 `docs/architecture.md`、`docs/getting-started.md`、`docs/api.md`、`CHANGELOG.md`。

## v0.5.0 — 2026-07-28

- **feat(core)**: `ProxyService` 增 `host/port` 字段 + `with_host_port` builder。
- **refactor(core)**: 提取 `auth_guard` 共享 middleware（`/health` 免鉴权，`/v1/*` 与 `/__asw/status` 统一校验）。
- **feat(core)**: `GET /__asw/status` 管理端点 + `CircuitDto` 下沉；`core::daemon`（DaemonConfig + daemon.json + launchctl load/unload）。
- **feat(cli)**: `asw serve install/uninstall/status/stop`（launchd 常驻 + RunAtLoad + KeepAlive）。
- **feat(gui)**: `daemon_ctl` 取代内嵌 `proxy_ctl`（HTTP 状态查询 + launchctl 控制）；系统托盘（关窗到托盘 + 每工具快速切换 + 启停代理）+ `tauri-plugin-autostart` 开机自启。

## v0.4.0 — 2026-07-28

- **feat(core)**: `Core: Clone`（`Arc<dyn SecretStore>`）；`serve_with_shutdown` 优雅关停；`RequestLog/StatsRow/StatsGroupBy/Preset` serde derive。
- **feat(gui)**: Tauri v2 桌面 GUI（React + TS + Vite + Tailwind），四页 Providers/Proxy/Stats/Logs；18 个 Tauri command 直调 Core；代理内嵌同进程。

## v0.3.0 — 2026-07-27

- **feat(core)**: Anthropic→OpenAI 协议互转（tool_calls 聚合-再分片为 `input_json_delta`）。
- `proxy_state` 增 `model_override`、`target_protocol`（chat/messages）。

## v0.2.0 — 2026-07-26

- **feat**: 本地代理 `asw serve`（axum + reqwest + SSE 流式透传）。
- 路由 failover + 熔断（FAIL_THRESHOLD=3，COOLDOWN=60s）。
- `asw proxy use/status/clear` + `asw stats`；`request_logs` + `proxy_state` 表。

## v0.1.0 — 2026-07-24

- **feat**: provider 切换 CLI（add/ls/use/current/edit/rm/presets/import/backup/completion）。
- 三 adapter（claude_code/codex/opencode）+ keyring 系统钥匙串 + 切换前备份轮转。
- core 库 `agent-switch-core`（rusqlite Mutex/Send+Sync）。
