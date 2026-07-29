# 架构

## Workspace 结构

```
agent-switch/
├── crates/
│   ├── core/              # 核心库 agent-switch-core
│   │   └── src/
│   │       ├── models.rs          # Provider / ToolKind / ToolKind::as_str
│   │       ├── store/
│   │       │   ├── db.rs          # rusqlite: providers / proxy_state / request_logs / stats
│   │       │   └── secrets.rs     # SecretStore trait + KeyringStore / MockStore / FileMockStore
│   │       ├── adapters/
│   │       │   ├── claude_code.rs # 写 ~/.claude/settings.json (env.ANTHROPIC_*)
│   │       │   ├── codex.rs       # 写 ~/.codex/config.toml + auth.json
│   │       │   └── opencode.rs    # 写 ~/.config/opencode/opencode.json + auth.json
│   │       ├── presets.rs         # 内置预设（official / local-proxy）
│   │       ├── backup.rs          # 切换前备份（纳秒时间戳，保留 10 份）
│   │       ├── service.rs         # Core: 配置/切换/import/备份的主入口
│   │       ├── proxy/
│   │       │   ├── mod.rs         # ProxyService + build_router + serve/serve_with_shutdown
│   │       │   ├── forward.rs     # 转发 + failover + 熔断
│   │       │   ├── convert.rs     # Anthropic↔OpenAI 协议互转
│   │       │   ├── usage.rs       # SSE 尾部缓冲提取 token usage
│   │       │   ├── status.rs      # GET /__asw/status + StatusDto/CircuitDto
│   │       │   ├── auth.rs        # auth_guard middleware
│   │       │   └── testsupport.rs # MockUpstream + test_core
│   │       ├── daemon.rs          # DaemonConfig + daemon.json read/write + launchctl load/unload
│   │       └── error.rs           # CoreError (thiserror, Send+Sync)
│   ├── cli/               # asw 二进制
│   │   └── src/{main.rs, daemon.rs}   # clap 命令 + launchd plist 写入
│   └── gui/               # Tauri app
│       ├── src/                   # React + TS 前端（Vite + Tailwind）
│       │   ├── pages/             # ProvidersPage / ProxyPage / StatsPage / LogsPage
│       │   ├── components/        # Layout / Toast / ProviderForm / RoutesEditor / BackupsPanel
│       │   └── api.ts             # invoke() 封装
│       └── src-tauri/             # Rust 后端
│           ├── src/{lib,commands,daemon_ctl,dto,state,tray}.rs
│           ├── tauri.conf.json    # bundle + externalBin (asw sidecar)
│           └── binaries/asw-<triple>   # 构建产物（gitignored）
└── docs/
```

## 数据模型

- `Provider { id, tool, base_url, key_ref, extra, is_active }`，`(tool, id)` 复合主键。
- `proxy_state`：`routes`（主→备 provider id 列表）、`model_override`、`target_protocol`。
- `request_logs`：ts/endpoint/model/provider_id/status/tokens/duration/error。
- `stats_since(since, group_by)`：聚合 requests/tokens/errors/avg_duration。

`Core: Clone`（`Arc<dyn SecretStore>`），GUI 与 `ProxyService` 共享同一实例。

## 数据流

### CLI 切换 provider

```
asw use <id> --tool <t>
  → Core::use_provider(tool, id)
    → ensure_imported → db.get(provider) → keyring.get(key_ref)
    → backup（写 config 前备份）
    → adapter.apply(provider, key)   # 写工具配置文件
    → db.set_active(tool, id)
```

### 代理转发

```
POST /v1/chat/completions
  → auth_guard（auth_token 校验，若设置）
  → forward::handle
    → db.get_routes() → routes[0]  # 每请求重读，改路由立即生效
    → circuit_open? 跳过已熔断
    → convert（按 target_protocol 转换请求体）
    → reqwest 上游（流式透传）
    → 成功 record_success / 失败 record_failure（≥3 次 → 熔断 60s）
    → usage 提取 + db.insert_request_log
    → 失败 failover 到 routes[i+1]
```

### 协议互转

`target_protocol` 决定请求/响应转换方向：客户端协议 ↔ 上游协议。tool_use/tool_calls 参数聚合-再分片为 `input_json_delta`（流式）。详见 `proxy/convert.rs`。

### daemon（macOS）

```
asw serve install
  → cli::daemon::install(home, host, port, token, load=true)
    → 写 ~/Library/LaunchAgents/ai.agent-switch.serve.plist（ProgramArguments=asw serve --foreground …）
    → 写 $ASW_DATA_DIR/daemon.json {host,port,auth_token}
    → launchctl unload + load（幂等）

launchd 托管进程 = `asw serve --foreground` → ProxyService::serve（axum）
  GET /__asw/status → 读 db routes + circuits 内存态 → JSON
```

### GUI ↔ daemon

```
GUI Proxy 页「启动」
  → command proxy_start(app, host, port, token)
    → daemon_ctl::start(app, …)
      → app.shell().sidecar("asw").args(["serve","install",…]).status()   # sidecar 自装
    → 轮询 daemon_ctl::status_from_config()  # HTTP GET /__asw/status（带 daemon.json token）
```

`asw` 作为 Tauri sidecar 打入 .app（`bundle.externalBin`），GUI 从 Finder 启动时无需 `~/.cargo/bin` 在 PATH。sidecar 的 `current_exe()` = .app 内 asw，plist 引用正确。

## 测试

- core：`MockUpstream`（本地 mock 上游）+ `test_core(tempdir, providers)` + `FileMockStore`，覆盖转换/转发/熔断/状态端点/daemon 文件级。
- CLI：`assert_cmd` 集成测试 + daemon 文件级（不实际 launchctl load）。
- GUI：commands/daemon_ctl 纯函数单测 + vitest 前端逻辑。
- 真实 launchctl load / GUI 窗口冒烟留手动。
