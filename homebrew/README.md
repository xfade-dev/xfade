# homebrew-xfade

Homebrew tap for [xfade](https://github.com/xfade-dev/xfade).

> The contents of this directory (`homebrew/`) should be copied into a standalone repo `xfade-dev/homebrew-xfade` (Homebrew tap convention: repo name `homebrew-<name>`, formula under `Formula/`).

## Install

```bash
brew tap xfade-dev/homebrew-xfade
brew install xfade
```

## Update the formula (after each release)

1. Tag the main repo (`git tag vX.Y.0 && git push --tags`) and wait for the release workflow to produce `xfade-*-apple-darwin.tar.gz` + `SHA256SUMS`.
2. Replace the `REPLACE_WITH_*` placeholders in `Formula/xfade.rb` with the two macOS sha256 values from the release's `SHA256SUMS`, and update `version`.
3. Commit + push to this tap repo; users get the new version via `brew upgrade xfade`.
