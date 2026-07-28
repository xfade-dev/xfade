# Agent Switch Tauri GUI 设计（v0.4）

日期：2026-07-27
状态：已批准（方案 A）

## 背景

`asw` 已有完整 CLI（v0.1 provider 切换 / v0.2 本地代理 / v0.3 Anthropic→OpenAI 协议互转，83 测试）。项目最初即定为「CLI + GUI 双形态」，本阶段交付 GUI。core 库（`agent-switch-core`）已为库形态，`Core: Send + Sync`，公共 API 面经摸底确认可直接支撑 GUI（见附录 A）。

## 目标 / 非目标

### 目标
- Tauri v2 桌面 GUI（标准窗口），功能对齐全部现有 CLI 能力：provider 管理（增删改查/切换/presets/import/备份）、代理控制（内嵌 serve 启停、路由 failover 排序、model_override、target_protocol、熔断状态）、stats 与请求日志面板。
- 与 CLI 共享同一数据目录 / db / 钥匙串（同 `HOME` / `ASW_DATA_DIR` / `ASW_MOCK_SECRETS` 环境契约），可同时使用。
- 代理内嵌同进程运行：GUI 内按钮启停，优雅关停。

### 非目标（YAGNI）
- 系统托盘 / menubar 常驻形态（后续版本再议）。
- daemon / 远程管理 HTTP API。
- tauri-driver E2E 测试、自动更新、应用签名公证。
- 反向协议互转（OpenAI→Anthropic）等代理新能力（属另一条线）。

## 关键决策（已与用户确认）

| 决策点 | 选择 |
|---|---|
| 功能范围 | 全功能（对齐全部 CLI） |
| 前端栈 | React + TypeScript（Vite） |
| 应用形态 | 标准窗口（1100×720） |
| 代理运行 | 内嵌同进程（`build_router()` + 自管 listener + 优雅关停） |
| GUI ↔ core 集成 | Tauri commands 直调 `Core`（方案 A），无中间 HTTP/CLI 层 |

## 架构

```
crates/gui/                # Tauri v2 app（前端根）
├── package.json / vite.config.ts / index.html
├── src/                   # React+TS 前端
│   ├── pages/  ProvidersPage / ProxyPage / StatsPage / LogsPage
│   ├── components/  Table / Modal / Tabs / Toast 等少量手写组件（Tailwind）
│   └── api.ts             # invoke() 封装，TS 类型与 Rust DTO 一一对应
└── src-tauri/             # Rust crate（workspace member，GUI 后端）
    ├── tauri.conf.json / capabilities/
    └── src/
        ├── main.rs        # Core 初始化、状态注册、command 注册
        ├── commands.rs    # tauri::command 集合（spawn_blocking 包 Core 同步调用）
        ├── dto.rs         # 前端契约 DTO（serde Serialize）
        └── proxy_ctl.rs   # 代理状态机 Stopped/Running + 启停控制
```

workspace 根 `Cargo.toml` 的 `members` 增加 `crates/gui/src-tauri`。

### 数据流
- 前端 `invoke('cmd_name', args)` → command → `tauri::async_runtime::spawn_blocking` 内调 `Core` 同步方法 → 结果序列化 JSON 返回。
- 错误统一 `map_err(|e| e.to_string())`（`CoreError` 的 Display 已面向用户），前端 toast + 行内提示。
- 路由/覆盖修改走 `core.db().set_routes(...)`：运行中代理每次请求重读路由（forward.rs:182），**立即生效，无需重启**。
- 代理启动：`ProxyService::new(core.clone()).with_auth_token(t).build_router()`，自行 `TcpListener::bind` + `axum::serve(...).with_graceful_shutdown(oneshot)`；停止即发送 oneshot 并 await JoinHandle。运行状态（端口、auth token 是否设置）由 `proxy_ctl` 状态机维护并可供查询。

### Core 增量（三处小改，保持向后兼容）
1. **serde derive**：`RequestLog`、`StatsRow`、`StatsGroupBy`（db.rs）、`Preset`（presets.rs）补 `#[derive(Serialize)]`（`StatsGroupBy` 同时补 `Deserialize` 以便 command 参数直接接收）。`Provider`/`ToolKind` 已有。注意 `Circuit`（proxy/mod.rs:21-25）含 `std::time::Instant` **不能 derive Serialize**，不改 core，由 GUI 侧 `CircuitDto` 转换（`cooldown_until: Option<Instant>` → 剩余冷却秒数 `Option<u64>`）。
2. **`Core: Clone`**：`secrets: Box<dyn SecretStore>` 改为 `Arc<dyn SecretStore>`，derive `Clone`（`Database` 已 `Clone`，`PathBuf` 可 Clone）。使 GUI 与 `ProxyService` 共享同一 Core 实例。`with_paths` 签名定为 `Arc<dyn SecretStore>` 入参（不用泛型，避免 `Core` 泛化），CLI 及测试调用点从 `Box::new(...)` 机械改为 `Arc::new(...)`。同时 core 的 `Cargo.toml` 显式补齐自身用到的 tokio features（`sync`/`net`/`time` 等），不再依赖 reqwest 传递启用。
3. **优雅关停**：`ProxyService` 新增 `serve_with_shutdown(self, host, port, signal: impl Future<Output=()> + Send)`（内部即 `build_router` + bind + `with_graceful_shutdown`），CLI 的 `serve` 可继续用原签名（内部委托新方法，signal 为 ctrl-c 或 pending）。GUI 也可选择直接 `build_router()` 自管——两者同源，行为一致。

## Commands API 面（src-tauri）

统一约定：返回 `Result<T, String>`；所有 Core 调用经 `spawn_blocking`；参数/返回类型全部 `Serialize + Deserialize`（DTO 见下）。

Provider 管理：
- `list_providers(tool: Option<ToolKind>) -> Vec<Provider>`
- `add_provider(input: AddProviderInput) -> ()`（input 含 id/tool/base_url/Option/key/Option/extra JSON）
- `update_provider(input: UpdateProviderInput) -> ()`（编辑含 extra 合并，等价 CLI `--set`）
- `remove_provider(tool: ToolKind, id: String) -> ()`
- `use_provider(tool: ToolKind, id: String) -> ()`
- `current_provider(tool: ToolKind) -> Option<Provider>`
- `list_presets(tool: ToolKind) -> Vec<PresetDto>`
- `import_provider(tool: ToolKind) -> Option<Provider>`（直接调 `core.import(tool)`，覆盖刷新 imported 快照）

备份（对齐 CLI 能力：ls / restore；保存由 `use_provider` 自动触发，不提供手动保存）：
- `list_backups(tool: ToolKind) -> Vec<String>`（路径列表）
- `restore_backup(tool: ToolKind, path: String) -> ()`

代理控制：
- `proxy_start(host: String, port: u16, auth_token: Option<String>) -> ProxyStatus`
- `proxy_stop() -> ProxyStatus`
- `proxy_status() -> ProxyStatus`（running/port/auth_enabled/各 provider 熔断 `Vec<CircuitDto>`）
- `get_routes() -> Option<RoutesDto>`（routes/model_override/target_protocol）
- `set_routes(routes: Vec<String>, model_override: Option<String>, target_protocol: String) -> ()`
- `clear_routes() -> ()`

统计与日志：
- `stats(since: String, by: StatsGroupBy) -> Vec<StatsRow>`（since 为 RFC3339，前端由 24h/7d/30d 换算）
- `recent_logs(limit: usize) -> Vec<RequestLog>`

DTO（dto.rs）：`AddProviderInput`、`UpdateProviderInput`、`PresetDto`、`ProxyStatus`、`CircuitDto`、`RoutesDto`。core 已 `Serialize` 的类型直接透传。

## 前端页面

1. **Providers 页**：Claude Code / Codex / OpenCode 三 Tab；列表列：名称、base_url（官方显示 "official"）、active 标记；行操作：切换 / 编辑 / 删除；页级操作：新增 provider（表单含 preset 下拉预填 base_url/extra、key 输入）、从工具导入（import）、备份子面板（折叠区：列表 + 恢复，恢复需二次确认；保存由切换时自动触发）。
2. **Proxy 页**：serve 控制卡（host/port/auth-token 输入 + 启停按钮 + 运行状态灯）；路由编辑器（候选 provider 多选加入，主→备顺序上下移动）；`model_override` 输入；`target_protocol` 单选（chat / messages）；熔断状态表（provider / fails / cooldown_until，运行时可刷新）。
3. **Stats 页**：时间范围（24h/7d/30d）+ 分组（provider/model）切换；表格：requests / prompt_tokens / completion_tokens / errors / avg_duration_ms。
4. **Logs 页**：最近 200 条请求日志（ts/endpoint/model/provider/status/tokens/duration/error），非 2xx 行高亮，手动刷新。

UI：Tailwind CSS + 手写基础组件（Table/Modal/Tabs/Toast/Badge），不引重型组件库。导航：左侧栏四页切换。

## 错误处理

- command 层 `CoreError → String` 上抛；前端统一 toast，表单场景行内提示。
- 端口占用、钥匙串拒绝、active provider 删除拒绝等场景错误信息原样透传。
- 代理运行中改路由 / 切 provider 无需重启；`proxy_stop` 幂等（Stopped 状态调用直接返回现状）。
- 恢复备份、删除 provider、clear 路由前端二次确认。

## 测试

- **core 增量**：现有 83 测试保持绿；新增 `serve_with_shutdown` 集成测试（起→/health→关停→端口可复用）；`Core: Clone` 编译与行为测试。
- **commands 层**：作为普通 Rust 函数单测（`Core::with_paths` + `FileMockStore` + tempdir，同 CLI 测试模式）；代理状态机启停/幂等测试。
- **前端**：vitest 覆盖 api 封装序列化与路由编辑器排序逻辑。
- **冒烟**：`cargo tauri dev` 手动验证：三工具切换、import、代理启停 + 路由热改 + stats/logs 展示（上游不可达时用 MockUpstream 思路本地起 mock 验证）。

## 分期

| 期 | 内容 | 验收 |
|---|---|---|
| G1 | core 增量（Serialize derive、`Core: Clone`、`serve_with_shutdown`、core Cargo.toml 显式补齐 tokio features）+ 测试 | 83+N 测试绿、clippy/fmt 净 |
| G2 | Tauri 脚手架 + commands/dto/proxy_ctl + commands 单测 | `cargo tauri dev` 起空壳窗口，commands 测试绿 |
| G3 | Providers 页（含 presets/import/备份子面板） | 页内完成 provider 全流程 |
| G4 | Proxy 页（启停 + 路由编辑 + 熔断展示） | 代理启停与热改路由可用 |
| G5 | Stats + Logs 页 + 整体冒烟 | 四页齐备，冒烟通过 |

## 风险

- **钥匙串 ACL**：Tauri app 与 CLI 二进制是不同的 keychain 访问主体，macOS 上首次读取 CLI 写入的条目可能弹授权框。缓解：冒烟阶段确认；文档说明；`ASW_MOCK_SECRETS=1` 兜底。
- **Tauri 构建链**：首次需安装 `tauri-cli` 与前端依赖（node/npm）；macOS 打包 ad-hoc 签名即可。
- **同步阻塞**：Core 方法全同步（SQLite/文件/keyring），必须 `spawn_blocking`，避免卡 Tauri 主线程——commands 层统一封装保证。
- **feature 隐式启用**：core 的 tokio feature 不含 `sync`/`net`/`time`，src-tauri 自己的 Cargo.toml 需显式声明。

## 附录 A：core 公共 API 摸底（关键引用）

- `Core::with_paths(home, data_dir, secrets)`（service.rs:18）；`Core::for_current_user()`（service.rs:29，固定 KeyringStore）
- 方法：`add_provider` :40 / `update_provider` :49 / `list` :62 / `current` :66 / `remove` :70（active 拒绝）/ `use_provider` :83 / `ensure_imported` :120 / `import` :134 / `db` :147 / `backups` :155 / `restore_backup` :159
- 代理：`ProxyService::new` / `with_auth_token` / `build_router(self) -> Router`（proxy/mod.rs:35/44/97）；`serve`（:86，无优雅关停——本次增量）；`circuit_status`（:82）
- 路由：`set_routes`（db.rs:208）/ `get_routes`（:241）/ `clear_routes`（:259）；`RoutesConfig = (Vec<String>, Option<String>, String)`
- 统计：`stats_since`（db.rs:287）/ `recent_request_logs`（:319）
- secrets：`SecretStore: Send + Sync`（secrets.rs:5）；`KeyringStore` :12 / `FileMockStore` :64；env 切换逻辑在 CLI `build_core`（cli/main.rs:154-172），GUI 复刻
- serde 现状：`Provider`/`ToolKind` 有 Serialize；`RequestLog`/`StatsRow`/`StatsGroupBy`/`Preset` 无（本次补 derive）；`Circuit` 无且含 `Instant` 不可 derive（GUI 侧 `CircuitDto` 转换）
- 错误：`CoreError`（error.rs:3-34）`Send + Sync`，无 Serialize
