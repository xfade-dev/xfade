# homebrew-agent-switch

Homebrew tap for [agent-switch](https://github.com/<user>/agent-switch)。

> 此目录（`homebrew/`）的内容应复制到独立仓库 `<user>/homebrew-agent-switch`（Homebrew tap 约定：仓库名 `homebrew-<name>`，formula 放 `Formula/`）。

## 安装

```bash
brew tap <user>/homebrew-agent-switch
brew install agent-switch
```

## 更新 formula（每次 release 后）

1. 在主仓库打 tag `git tag vX.Y.0 && git push --tags`，等 release workflow 产出 `asw-*-apple-darwin.tar.gz` + `SHA256SUMS`。
2. 用 release 中 `SHA256SUMS` 的两个 macOS sha256 替换 `Formula/agent-switch.rb` 的 `REPLACE_WITH_*`，并更新 `version`。
3. commit + push 到本 tap 仓库；用户 `brew upgrade agent-switch` 即得新版。
