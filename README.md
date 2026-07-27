# asw — Agent Switch

## 简介

`asw` 是一个为 AI 编码工具（Claude Code / Codex / OpenCode）切换 API provider 的小工具。v0.2 新增本地代理模式：`asw serve` 把路由 provider 暴露为 OpenAI/Anthropic 兼容的本地端点，支持热切换、failover 与熔断、请求统计。所有配置写入 `$HOME/.config/agent-switch/agent-switch.db`。

## 安装

需要 Rust 稳定工具链。在仓库根目录执行：

```bash
cargo build --release        # 产物：target/release/asw
cargo install --path crates/cli   # 可选：安装到 ~/.cargo/bin
```

环境契约：`HOME` 决定工具配置根（如 `~/.claude`）；`ASW_DATA_DIR` 覆盖自身数据目录（默认 `$HOME/.config/agent-switch`）；`ASW_MOCK_SECRETS=1` 用文件 mock 替代系统钥匙串（仅测试/冒烟）。

## 命令速查

v0.1：`asw add --tool <t> --name <n> --base-url <u> --key <k> [--set k=v]`、`asw ls [--tool]`、`asw use <n> [--tool]`、`asw edit <n> [...]`、`asw rm <n>`、`asw presets`、`asw current`、`asw import [--tool]`、`asw backup [ls|save]`、`asw completion <shell>`。

v0.2 代理：`asw serve [--port 24860] [--host 127.0.0.1] [--auth-token T]`、`asw proxy use <n> [...]`、`asw proxy status`、`asw proxy clear`、`asw stats [--since 7d] [--by provider|model]`。

## 代理模式上手

1. 先用 v0.1 命令登记 provider：`asw add --tool codex --name yy --base-url http://up:4000 --key sk-x`。
2. 设置路由（主→备，逐个 failover）：`asw proxy use yy`，查看 `asw proxy status`。
3. 启动代理：`asw serve --port 24860 &`，健康检查 `curl http://127.0.0.1:24860/health`。
4. 让工具走本地端点：`asw add --tool codex --name local --base-url http://127.0.0.1:24860/v1`（交互式选 `local-proxy` 预设亦可），再 `asw use local`。
5. 发请求并查统计：`curl -X POST http://127.0.0.1:24860/v1/chat/completions -H "Authorization: Bearer any" -d '{"model":"gpt-x","messages":[]}'`，再 `asw stats`。

## GUI

Tauri v2 桌面 GUI（功能对齐 CLI）：

```bash
cd crates/gui
cargo tauri dev          # 开发模式，热重载
cargo tauri build        # 产物：target/release/bundle/ 下的 .app / .dmg
```

环境契约与 CLI 一致（`HOME` / `ASW_DATA_DIR` / `ASW_MOCK_SECRETS`），GUI 与 CLI 共享同一数据目录与钥匙串，可同时使用。四页：Providers（增删改查/切换/import/备份）、Proxy（内嵌 serve 启停 + 路由 failover 编辑 + 熔断状态）、Stats（按 provider/model 聚合）、Logs（最近 200 条请求）。
