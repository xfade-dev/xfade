# Agent Switch 打包发布设计（v0.6b / Phase B）

日期：2026-07-29
状态：已批准（按推荐决策）
范围：v0.6 发布硬化第二个子项目（打包发布）。技术债（A）已完成，文档（C）为后续。

## 关键决策（按推荐）

| 决策点 | 选择 | 理由 |
|---|---|---|
| CI/release | 手写 GitHub Actions | CLI 跨平台二进制 + GUI dmg + Homebrew 三线，单一 workflow 透明可控；cargo-dist 不懂 Tauri |
| crates.io | 仅设 Cargo.toml metadata，暂不发布 | 主路径是 Homebrew/release binary/dmg；core 内部库不宜过早暴露公开 API |
| Homebrew | 独立 tap 仓库 `homebrew-agent-switch` | 开源标准做法，formula 从 release 拉 binary，与源码解耦 |
| GUI 打包 | 含 asw Tauri sidecar | GUI 自装 daemon，彻底解决 Finder PATH 问题，产品自包含 |

## 目标 / 非目标

### 目标
- Cargo.toml metadata 补全（root + core + cli + gui），publish-ready 但不实际 `cargo publish`。
- CI workflow（push/PR：test+clippy+fmt+tsc+vitest）。
- Release workflow（tag 触发：CLI 多平台二进制 + macOS GUI .dmg + checksums + GitHub Release）。
- Homebrew formula（独立 tap 仓库，源 release binary）。
- GUI sidecar：`asw` 打包进 .app，daemon_ctl 改用 sidecar 执行 `asw serve install/uninstall`，GUI 自装 daemon。
- RELEASE.md 打包/发布流程文档。

### 非目标
- 实际 `cargo publish`（仅 publish-ready）。
- 代码签名/公证（ad-hoc 签名即可，正式公证后续；Homebrew 用户不需公证）。
- Linux/Windows GUI 打包（仅 macOS .dmg；CLI 仍跨平台）。
- 自动更新（Tauri updater 后续）。

## Cargo.toml metadata

根 `Cargo.toml` 新建 `[workspace.package]` 段（当前只有 `[workspace]` + `[workspace.dependencies]`）：
```toml
[workspace.package]
version = "0.6.0"
edition = "2021"
license = "MIT OR Apache-2.0"
repository = "https://github.com/<user>/agent-switch"
homepage = "https://github.com/<user>/agent-switch"
keywords = ["ai", "cli", "anthropic", "openai", "proxy"]
categories = ["command-line-utilities", "config"]
```
各 crate `[package]`（core/cli/gui）用 `version.workspace = true` / `license.workspace = true` 等继承。**gui crate 现有占位值须清理**：`crates/gui/src-tauri/Cargo.toml` 当前 `repository = ""`、`license = ""`、`authors = ["you"]`、`version = "0.1.0"`，全部改为 workspace 继承或正式值。`<user>` 在 B1 阶段就确定，避免后期多处遗漏。`tauri.conf.json` 的 `version: "0.1.0"` 同步改为 `"0.6.0"`。

## CI workflow（.github/workflows/ci.yml）

push/PR 触发：
- job `test`（ubuntu-latest）：`cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`。
- job `frontend`（ubuntu-latest, setup-node, `cd crates/gui && npm ci && npx tsc --noEmit && npm test`）。
- 缓存 cargo + npm。

## Release workflow（.github/workflows/release.yml）

`v*` tag 触发：
- job `cli`（matrix: macos-14[aarch64], macos-13[x86_64], ubuntu-22.04[x86_64], windows-latest[x86_64]）：`cargo build --release -p asw` → 打包 `asw-<triple>.tar.gz`（win 用 zip）+ sha256。
- job `gui`（macos-14）：先 `cargo build --release -p asw` 复制到 `crates/gui/src-tauri/binaries/asw-aarch64-apple-darwin`（sidecar），`npm ci`，`cargo tauri build` → 产出 `.dmg`/`.app`。
- job `release`（needs [cli, gui]）：上传所有产物到 GitHub Release（softprops/action-gh-release），附 checksums 文件。

## Homebrew formula（独立 tap 仓库 homebrew-agent-switch/Formula/agent-switch.rb）

```ruby
class AgentSwitch < Formula
  desc "Switch API providers for AI coding tools (Claude Code/Codex/OpenCode)"
  homepage "https://github.com/<user>/agent-switch"
  url "https://github.com/<user>/agent-switch/releases/download/v0.6.0/asw-aarch64-apple-darwin.tar.gz"
  sha256 "<release-sha256>"
  version "0.6.0"
  def install
    bin.install "asw"
  end
  test do
    assert_match "asw", shell_output("#{bin}/asw --version")
  end
end
```
macOS aarch64/x86_64 分别（formula 用 `on_arm do ... on_intel do ...` 选 url）。我写好 formula 模板（占位 url/sha，发布后填）+ tap README。CLI 安装：`brew install <user>/homebrew-agent-switch/agent-switch`。

## GUI sidecar

**tauri.conf.json**（现有 `bundle` 段含 `active/targets/icon`，兼容）：
- `identifier` 由脚手架默认 `com.tauri.dev` 改为正式值 `ai.agent-switch.gui`。
- `bundle.targets` 由 `"all"` 改为 `["dmg", "app"]`（非目标明确仅 macOS，避免意外构建 deb/appimage/msi）。
- `bundle.externalBin` 加 `["binaries/asw"]`。
- `version` 同步 `0.6.0`。

Tauri 构建时找 `crates/gui/src-tauri/binaries/asw-<target-triple>`（无扩展名 unix / `.exe` win）打入 .app；运行时 `app.shell().sidecar("asw")` 解析到 bundled 二进制。

**sidecar 构建**：`crates/gui/build-sidecar.sh`（本地 dev 用）+ release workflow 步骤。脚本用 `rustc -vV` 解析 host triple（dev 环境 triple 不一定与 release 一致）：
```bash
#!/usr/bin/env bash
set -e
TRIPLE=$(rustc -vV | awk '/^host/ {print $2}')
cargo build --release -p asw
mkdir -p crates/gui/src-tauri/binaries
cp target/release/asw crates/gui/src-tauri/binaries/asw-$TRIPLE
```

**tauri-plugin-shell 注册**：`crates/gui/src-tauri/Cargo.toml` 加 `tauri-plugin-shell = "2"`；`lib.rs` Builder 链加 `.plugin(tauri_plugin_shell::init())`（与现有 autostart/log 并列）；`capabilities/default.json`（当前仅 `core:default`）加 `"shell:allow-execute"`。

**daemon_ctl 改造**（`start`/`stop` 改用 sidecar 执行 `asw serve install/uninstall`）：
- `daemon_ctl::start(app: &AppHandle, host, port, auth_token)` → `app.shell().sidecar("asw")?.args(install_args(host,port,auth_token)).status()` 执行 `asw serve install`。`stop(app)` → `args(["serve","uninstall"])`。
- `install_args`/`uninstall_args` 为**新建**的纯函数（当前 daemon_ctl 无此函数；v0.5 的 launchctl-only 重构时移除了），单测覆盖。
- proxy_start/stop 命令把 `AppHandle`（tauri command 可直接收 `app: AppHandle` 参数）透传给 daemon_ctl。
- **install≠start 语义处理**：`asw serve install` 写 plist + load。为让 GUI "start" 可重复调用（已运行时再点启动），`core::daemon::install`（`load=true` 时）改为 **先 unload 再 load**（幂等），避免 `launchctl load` 对已加载 plist 报错。sidecar 的 `current_exe()` = .app 内 asw 路径 → plist 引用正确，daemon 跑 bundled asw。
- auth_token：从 GUI 前端 Proxy 页输入透传给 `install_args`（与 v0.4 一致）；默认 None。
- core::daemon::load/unload 保留（CLI install/uninstall 用）；GUI 走 sidecar 路径。

**capability**（`capabilities/default.json`）：
```json
"permissions": ["core:default", "shell:allow-execute"]
```
（精确到 sidecar scope 更安全，初版用 allow-execute，后续收紧。）

## RELEASE.md

文档化：版本号约定、`git tag v0.x.0` → CI 自动构建发布、发布后更新 Homebrew formula 的 url/sha256、sidecar 本地构建步骤、ad-hoc 签名说明。

## 测试
- sidecar 路径无法在单测里跑（需 tauri 运行时 + bundled 二进制）；daemon_ctl 的 sidecar 调用通过**新建**的 `install_args`/`uninstall_args` 纯函数单测覆盖（当前不存在，需新建）。
- `core::daemon::install` 幂等（unload-then-load）单测：install 两次不报错（load 步骤在测试中 `load=false` 跳过 launchctl，仅校验 plist/daemon.json 写入幂等）。
- CI workflow 本身在 GitHub 验证（本地无法跑 Actions）。
- Homebrew formula 本地 `brew install --build-from-source ./Formula/agent-switch.rb` 验证（发布后）。
- 现有 96 测试保持绿；B 不改 core 业务逻辑。daemon_ctl 现有 2 测试（status/config）不涉及 start/stop，保持。

## 分期
- B1：Cargo.toml metadata + version 0.6.0
- B2：CI workflow（ci.yml）
- B3：Release workflow（release.yml）+ sidecar 构建脚本
- B4：GUI sidecar（tauri.conf + tauri-plugin-shell + daemon_ctl 改造 + capability）
- B5：Homebrew formula + tap README
- B6：RELEASE.md + 冒烟（本地 cargo tauri build 验证 sidecar 打包）

## 风险
- `<user>` GitHub 用户名未定 → metadata/formula/tap 用占位，发布前替换。
- sidecar 二进制 triple 命名错误 → tauri 找不到 sidecar 报错；release workflow 必须用正确 `rustc -vV` 解析的 triple 命名。
- `tauri-plugin-shell` capability 过宽（allow-execute）→ 安全取舍，初版接受，后续收紧到 sidecar scope。
- Homebrew formula sha256 需发布后填 → 流程文档说明。
- 实际 GitHub Actions 跑通需 push 到 GitHub → 本地只能校验 YAML 语法 + 本地 cargo tauri build。
