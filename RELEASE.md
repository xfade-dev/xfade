# 打包与发布

## 版本约定

- 语义化版本 `MAJOR.MINOR.PATCH`，crate 版本统一在根 `Cargo.toml` 的 `[workspace.package] version`。
- 发版前同步：`workspace.package.version` + `crates/gui/src-tauri/tauri.conf.json` 的 `version`。

## CI

- `.github/workflows/ci.yml`：push/PR 跑 fmt + clippy + cargo test + 前端 tsc + vitest。
- `.github/workflows/release.yml`：`git tag vX.Y.Z` 触发：
  - CLI 多平台二进制（macOS arm/intel、linux、windows）+ sha256。
  - macOS GUI `.dmg`/`.app`（含 asw sidecar）。
  - 汇总到 GitHub Release + `SHA256SUMS`。

## 发版流程

```bash
# 1. 确认版本号已更新（workspace.package.version + tauri.conf.json version）
# 2. 提交并打 tag
git commit -am "release: v0.6.0"
git tag v0.6.0
git push origin main --tags
# 3. release workflow 自动构建并发布 GitHub Release
```

## 发布后：更新 Homebrew formula

1. 从 GitHub Release 下载 `SHA256SUMS`，取两个 macOS sha256。
2. 到 tap 仓库 `homebrew-agent-switch`，更新 `Formula/agent-switch.rb`：`version` + 两个 `sha256`。
3. `git commit -am "v0.6.0" && git push`；用户 `brew upgrade agent-switch`。

## GUI sidecar（本地 dev/build 前）

`cargo tauri dev`/`build` 前，tauri-build 校验 `externalBin`（`binaries/asw-<triple>`）存在。本地先构建 sidecar：

```bash
bash crates/gui/build-sidecar.sh   # cargo build --release -p asw + 复制到 binaries/asw-<host-triple>
```

CI release workflow 内已含等价步骤，无需手动。

## 本地打包 GUI（冒烟）

```bash
cd crates/gui
bash build-sidecar.sh
npx tauri build        # 产出 src-tauri/target/release/bundle/{dmg,macos}/
```

## 签名

- 当前 ad-hoc 签名（macOS 本地可跑，分发时 Gatekeeper 会提示）。正式分发需 Developer ID 签名 + 公证，后续配置 `tauri.conf.json` 的 `signingIdentity` + notarization。
- Homebrew 用户不受 Gatekeeper 影响（brew 安装的二进制）。

## crates.io

当前仅设 Cargo.toml metadata（publish-ready），未实际 `cargo publish`。需要时：
```bash
cargo publish -p agent-switch-core   # 先发 core（CLI/GUI 依赖）
cargo publish -p asw                  # cargo install agent-switch-asw? （bin name asw）
```
（确认 crate 名可用后再发；`asw` 名可能已被占用，需核查。）
