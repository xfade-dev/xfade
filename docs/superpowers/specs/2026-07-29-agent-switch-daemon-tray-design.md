# Agent Switch 代理常驻 + 托盘设计（v0.5 / Phase 5a）

日期：2026-07-29
状态：已批准

## 背景

v0.2 起代理以 `asw serve` 前台运行，关闭终端即死；v0.4 GUI 内嵌代理但关窗即停。日常使用短板：代理无法常驻、无开机自启、GUI 无托盘常驻形态。本阶段交付 CLI daemon（macOS launchd）+ GUI 托盘控制同一 daemon，使代理脱离终端/GUI 存活并开机自启。

## 目标 / 非目标

### 目标
- CLI：`asw serve install/uninstall/status/stop`，daemon 经 launchd 常驻 + RunAtLoad 自启 + KeepAlive 崩溃重启。
- 代理新增 `GET /__asw/status` 管理端点（auth_token 校验），供 CLI status 与 GUI 查询运行/路由/熔断。
- GUI：托盘（关窗到托盘 + 菜单含每工具快速切换 + 定时刷新），Proxy 页由 v0.4 内嵌 spawn 改为 daemon 控制。
- GUI 开机自启（tauri-plugin-autostart）。

### 非目标（YAGNI）
- Linux(systemd)/Windows(计划任务) daemon 化：后续 follow-up。前台 `asw serve` 与 GUI 托盘关窗行为仍跨平台。
- 反向协议互转、配置云同步、daemon 多实例。
- daemon 配置热改（改 host/port/token 需 uninstall→install）。

## 关键决策（已确认）

| 决策点 | 选择 |
|---|---|
| 常驻进程位置 | CLI 独立 daemon（launchd），GUI 托盘经 HTTP 状态端点控制同一 daemon |
| daemon 机制 | macOS launchd plist（RunAtLoad + KeepAlive） |
| 托盘菜单 | 含每工具快速切换 provider 子菜单 |
| 开机自启 | daemon：plist RunAtLoad；GUI：tauri-plugin-autostart |
| 管理端点鉴权 | auth_token 校验（若设置），与 /v1/* 同策略 |

## 架构

```
daemon（launchd 托管的 `asw serve --foreground` 进程）
  ├─ axum router：/v1/*（代理） + /__asw/status（管理）
  ├─ circuits 内存态（ProxyService state）
  └─ 日志 → $ASW_DATA_DIR/serve.log

CLI asw serve install/uninstall/status/stop
  └─ 写 ~/Library/LaunchAgents/ai.agent-switch.serve.plist + $ASW_DATA_DIR/daemon.json + launchctl

GUI（tauri app）
  ├─ daemon_ctl：HTTP 查 /__asw/status + 子进程 asw serve install/uninstall
  ├─ 托盘：TrayIconBuilder + 关窗到托盘 + 菜单（快速切换）+ 定时刷新
  └─ Proxy 页：状态/启停/路由/熔断 均经 daemon_ctl
```

数据流：
- 启停 daemon = `launchctl load/unload`（CLI 子命令或 GUI 子进程触发）。
- 状态查询 = `GET /__asw/status`（带 daemon.json 里的 auth_token）。
- 路由/覆盖修改 = `db().set_routes()`（写 db，daemon 每请求重读，立即生效）。
- 快速切换 provider = `Core::use_provider(tool,id)`（GUI 自有 Core 实例，写工具配置文件；与 daemon 无共享状态冲突）。

## CLI daemon

`asw serve` 子命令化（保留无参=前台）：

目标 clap 结构（`Cmd::Serve` 由 struct variant 改为带 subcommand）：
```rust
Serve {
    #[command(subcommand)]
    cmd: Option<ServeCmd>,
}
enum ServeCmd {
    /// 前台运行（plist 调用）
    Foreground { port: u16, host: String, auth_token: Option<String> },
    /// 安装为 launchd 常驻 daemon
    Install { port: u16, host: String, auth_token: Option<String> },
    /// 卸载 daemon
    Uninstall,
    /// 停止 daemon（uninstall 别名）
    Stop,
    /// 查询状态
    Status,
}
```
`asw serve`（无子命令）= 等价 `Foreground` 默认参数（24860/127.0.0.1/无 token），保持 v0.2 行为不变。`--foreground` 即 `Foreground` 子命令。

具体子命令：
- `asw serve [--port --host --auth-token]` / `asw serve --foreground`：原前台阻塞，不变。plist 调用 `--foreground`。
- `asw serve install [--port 24860] [--host 127.0.0.1] [--auth-token T]`：
  - 写 `~/Library/LaunchAgents/ai.agent-switch.serve.plist`：`Label=ai.agent-switch.serve`，`ProgramArguments=["<asw bin>","serve","--foreground","--port",P,"--host",H,("—auth-token",T)?]`，`RunAtLoad=true`，`KeepAlive=true`，`StandardOutPath/StandardErrorPath=$ASW_DATA_DIR/serve.log`。
  - 写 `$ASW_DATA_DIR/daemon.json`：`{host,port,auth_token}`，供 status/GUI 读取。
  - `launchctl load <plist>`。
  - 幂等：已安装则先 unload 再覆盖。
- `asw serve uninstall`：`launchctl unload` + 删 plist + 删 daemon.json。
- `asw serve stop`：`uninstall` 别名。
- `asw serve status`：读 daemon.json → 若无则报"未安装"；`GET /__asw/status`（带 token）→ 打印 运行/端口/路由主→备/model_override/target_protocol/各熔断；HTTP 不通则报"daemon 已加载但未响应（查 serve.log）"。

二进制路径：plist 用当前 `asw` 绝对路径（`std::env::current_exe`），避免 PATH 问题。

## 代理管理端点（core 增量）

core 改动汇总：
1. **`ProxyService` 增 `host: String` + `port: u16` 字段**：当前 `ProxyService`（proxy/mod.rs:27-32）只有 `core/client/circuits/auth_token`，host/port 仅作 `serve` 参数不入结构。新增字段，构造链改为 `ProxyService::new(core).with_host_port(host,port).with_auth_token(token)`；`serve`/`serve_with_shutdown` 改用自身字段 bind。CLI/GUI/测试调用点同步。
2. **`GET /__asw/status` 路由**：`ProxyService::build_router` 增 `GET /__asw/status`。handler 读 `State<Arc<ProxyService>>`：返回
  ```json
  {
    "running": true,
    "host": "127.0.0.1", "port": 24860, "auth_enabled": true,
    "routes": ["yy","bak"], "model_override": "gpt-x", "target_protocol": "chat",
    "circuits": [{"provider_id":"yy","fails":2,"cooldown_remaining_secs":null}]
  }
  ```
  - `running` 恒 true（端点活着即运行）；`host/port/auth_enabled` 来自新增字段 + `auth_token.is_some()`；routes/model_override/target_protocol 来自 `core.db().get_routes()`；circuits 来自 `ProxyService.circuits`（换算 `cooldown_until` → 剩余秒）。
- 鉴权共享 layer：现有 auth_token 校验散在 forward handler 内部不可复用；提取为 axum middleware `auth_guard`，作用于 `/v1/*` 与 `/__asw/status`，`/health` 不挂（liveness 免鉴权）。未设置 auth_token 时放行。
- core 复用：`Circuit`→DTO 换算逻辑下沉到 core（`proxy::status_dto`），端点与 GUI 共用；GUI `dto.rs` 的 `circuit_dto` 改为 re-export 或删除。

## GUI 托盘

- 依赖：tauri v2 内置 tray（`tauri::tray::TrayIconBuilder`）+ `tauri-plugin-autostart`。
- 关窗到托盘：`on_window_event` 拦截 `WindowEvent::CloseRequested` → `api.prevent_close()` + `window.hide()`。
- 托盘菜单：
  - 显示窗口
  - 代理：`运行中 :端口` / `已停止`（动态文本）→ 子项 `启动`/`停止`
  - 切换 Claude Code ▸ [该工具 provider 列表，✓ 标当前]
  - 切换 Codex ▸ […]
  - 切换 OpenCode ▸ […]
  - 退出（仅退 GUI；daemon 常驻不受影响）
- 定时刷新：tauri::async_runtime 每 5s 查 `/__asw/status`，重建菜单（状态项 + provider 列表）；HTTP 不通则状态显示"未运行"。
- 启停代理：托盘"启动"=`asw serve install`（用上次 daemon.json 或默认参数）子进程；"停止"=`asw serve uninstall` 子进程。
- 快速切换：菜单项点击 → tauri command `use_provider(tool,id)` → GUI 自有 `Core::use_provider`。
- GUI 自启：托盘菜单或设置项开关 `tauri-plugin-autostart`。

## GUI Proxy 页改造

- v0.4 `proxy_ctl`（内嵌 spawn + oneshot 关停）替换为 `daemon_ctl`：
  - `status()` → HTTP `GET /__asw/status`（带 daemon.json 的 token）。
  - `start(host,port,auth_token)` → 子进程 `asw serve install ...`。
  - `stop()` → 子进程 `asw serve uninstall`。
- **tauri command 名称保持不变**（`proxy_start`/`proxy_stop`/`proxy_status`/`get_routes`/`set_routes`/`clear_routes`），仅 Rust 实现改为委托 `daemon_ctl`；前端 invoke 调用与 Proxy 页 UI 无需改动（`daemon_ctl` 为 Rust 内部模块）。
- 路由编辑、model_override、target_protocol 仍走 `set_routes`/`clear_routes`（db，daemon 立即生效）。
- 熔断表来自 `status().circuits`。
- v0.4 的 `Core: Clone` + `serve_with_shutdown` 保留（core 仍支持内嵌，供测试/未装 daemon 场景；GUI 不再用内嵌路径）。

## 自启

- daemon：plist `RunAtLoad=true`（install 即生效）。
- GUI：`tauri-plugin-autostart`（Mac 登录项）。

## 跨平台

- daemon/launchd 仅 macOS；`asw serve install` 在非 macOS 报错"daemon 仅支持 macOS（Linux/Windows 见 follow-up）"。
- 前台 `asw serve` + GUI 托盘关窗到托盘 跨平台可用。

## 错误处理

- `install`：plist 写入失败、launchctl load 失败 → 错误上抛（CLI 退出码非零；GUI toast）。
- `status`：daemon.json 缺失 → "未安装"；HTTP 不通 → "已加载未响应，查 serve.log"。
- `uninstall`：未安装幂等（直接成功）。
- auth_token 明文存于 plist ProgramArguments 与 daemon.json → 文档注明（本地用户可读；存 keyring 让 launchd 取不便，权衡取明文）。

## 测试

- **core**：`/__asw/status` 端点测试（起服务→curl status→断言字段 + auth_token 401 校验 + circuits 换算）。
- **CLI**：`install/uninstall` 用 tempdir HOME + tempdir `~/Library/LaunchAgents`（HOME 指向 tempdir），校验 plist/daemon.json 内容与增删；不实际 `launchctl load`（CI 不可用），实际加载留冒烟。`status` 对测试内 spawn 的前台 serve（带 auth_token）curl 验证。
- **GUI**：`daemon_ctl` HTTP 查询 + install/uninstall 子进程参数构造单测；托盘菜单结构（provider 列表→菜单项）单测；定时刷新逻辑单测。
- **冒烟**：`asw serve install` → `asw serve status` → `curl /v1/chat/completions` → GUI 托盘启停/快速切换 → `asw serve uninstall`。

## 分期

| 期 | 内容 | 验收 |
|---|---|---|
| D1 | core `/__asw/status` 端点 + circuits DTO 下沉 + 测试 | core 测试绿 |
| D2 | CLI `serve install/uninstall/status/stop` + daemon.json + 测试 | 文件级测试绿，clippy/fmt 净 |
| D3 | GUI `daemon_ctl` + Proxy 页改造为 daemon 控制 | Proxy 页经 daemon 可用 |
| D4 | GUI 托盘（关窗到托盘 + 快速切换菜单 + 定时刷新）+ autostart | 托盘功能可用 |
| D5 | 冒烟 + 文档 | 全链路通 |

## 风险

- launchctl 不可用于自动化测试 → 仅校验 plist/daemon.json 文件，实际 load/unload 留冒烟。
- auth_token 明文存 plist/daemon.json → 本地用户可读，文档注明。
- KeepAlive 崩溃重启可能掩盖 bug → `serve.log` 可查。
- 托盘菜单定时重建与 tauri 事件循环 → 用 `tauri::async_runtime` 定时器 + `tray.set_menu`，注意菜单项 id 映射。
- daemon 与 GUI 同时写 db（路由/active）→ SQLite Mutex 已保证，无新增冲突。
