# agent-switch

在 Claude Code / Codex / OpenCode 等 AI 编码工具之间切换 API provider，并提供本地代理（OpenAI/Anthropic 兼容端点、failover、协议互转）与 macOS 常驻 daemon。

CLI（`asw`）+ Tauri GUI 双形态，Rust 实现。

## 功能

- **Provider 切换**：为 Claude Code / Codex / OpenCode 增删改查 provider，一键切换写各自配置（settings.json / config.toml+auth.json / opencode.json），系统钥匙串存 key，切换前自动备份。
- **本地代理**：`asw serve` 起 OpenAI/Anthropic 兼容端点（`/v1/chat/completions`、`/v1/messages`、`/v1/responses`、`/v1/models`），支持路由 failover、熔断、token 统计、请求日志。
- **协议互转**：Anthropic↔OpenAI 双向转换（`target_protocol`），让客户端与上游协议解耦。
- **daemon（macOS）**：launchd 常驻 + 开机自启 + 崩溃重启；`/__asw/status` 管理端点。
- **GUI**：Tauri 桌面 app，四页（Providers / Proxy / Stats / Logs）+ 系统托盘（关窗到托盘、快速切换、启停代理），`asw` 作为 sidecar 自装 daemon。

## 安装

### CLI

```bash
# Homebrew（推荐，macOS）
brew tap <user>/homebrew-agent-switch
brew install agent-switch

# 或从 GitHub Release 下载二进制
# 或源码构建
cargo build --release -p asw   # 产物 target/release/asw
```

### GUI

从 GitHub Release 下载 `asw-gui_<version>_aarch64.dmg`（macOS Apple Silicon）拖入 Applications。或源码构建见下方「开发」。

## 快速开始

```bash
# 1. 添加一个第三方 provider（以 Codex 为例）
asw add kimi --tool codex --base-url https://api.moonshot.cn/v1 --key sk-xxx

# 2. 切换到它（写入 ~/.codex/config.toml + auth.json，自动备份原配置）
asw use kimi --tool codex

# 3. 查看当前
asw current --tool codex
```

三工具同理：`--tool claude-code|codex|open-code`。

## CLI 命令速查

```
asw add <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
asw ls [--tool <t>]
asw use <id> --tool <t>
asw current --tool <t>
asw edit <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
asw rm <id> --tool <t>
asw presets --tool <t>
asw import --tool <t>            # 从工具现有配置导入
asw backup ls|restore --tool <t>
asw completion <shell>
```

### 本地代理

```bash
asw serve [--port 24860] [--host 127.0.0.1] [--auth-token T]   # 前台运行
asw proxy use --routes yy,bak [--model-override gpt-x] [--target-protocol chat|messages]
asw proxy status
asw proxy clear
asw stats                  # 按 provider/model 聚合
```

### daemon（macOS）

```bash
asw serve install [--port --host --auth-token]   # 安装 launchd 常驻 + 开机自启
asw serve status                                 # 查询运行/路由/熔断
asw serve uninstall                              # 卸载
```

日志：`~/.config/agent-switch/serve.log`。

## GUI

```bash
cd crates/gui && npx tauri dev    # 开发
```

左侧栏四页：
- **Providers**：增删改查/切换/import/备份。
- **Proxy**：控制 daemon（启停）、路由 failover 编辑、熔断状态。
- **Stats**：24h/7d/30d × provider/model 聚合。
- **Logs**：最近 200 条请求日志。

系统托盘：关窗到托盘、启动/停止代理、每工具快速切换 provider、退出。GUI 开机自启。GUI 内置 `asw` sidecar，可自行 `serve install`（无需 CLI 前置安装）。

## 环境契约

- `HOME`：工具配置根。
- `ASW_DATA_DIR`：自身数据目录（db/backups/daemon.json），缺省 `$HOME/.config/agent-switch`。
- `ASW_MOCK_SECRETS=1`：用文件 MockStore 替代系统钥匙串（测试/冒烟）。

CLI 与 GUI 共享同一数据目录与钥匙串，可同时使用。

## 开发

Rust workspace：`crates/core`（库）+ `crates/cli`（`asw`）+ `crates/gui/src-tauri`（Tauri）。

```bash
cargo test                       # 全量测试
cargo clippy --all-targets --workspace -- -D warnings
cd crates/gui && npm install && npx tauri dev   # GUI 开发（需先构建 sidecar）
bash crates/gui/build-sidecar.sh               # 构建 asw sidecar 到 binaries/
```

详见 [docs/architecture.md](docs/architecture.md)、[docs/getting-started.md](docs/getting-started.md)、[docs/api.md](docs/api.md)、[CHANGELOG.md](CHANGELOG.md)、[RELEASE.md](RELEASE.md)。

## License

MIT OR Apache-2.0
