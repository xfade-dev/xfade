# homebrew-xfade

Homebrew tap for [xfade](https://github.com/xfade-dev/xfade).

> The contents of this directory (`homebrew/`) should be copied into a standalone repo `xfade-dev/homebrew-xfade` (Homebrew tap convention: repo name `homebrew-<name>`, formula under `Formula/`).

## Install

```bash
brew install xfade-dev/xfade/xfade
```

(Three segments: `<user>/<tap>/<formula>` — the repo `homebrew-xfade` normalizes to tap `xfade-dev/xfade`, and brew auto-taps it. The two-segment `brew install xfade-dev/xfade` does NOT work: brew looks for `xfade` in the official API, where third-party taps are absent.)

## Update the formula (after each release)

1. Tag the main repo (`git tag vX.Y.Z && git push --tags`) and wait for the release workflow to produce `xfade-*-apple-darwin.tar.gz` + `SHA256SUMS`.
2. In the tap repo, update `Formula/xfade.rb`: `version` + the two macOS `sha256` from the release's `SHA256SUMS`.
3. Commit + push to the tap repo (`master`); users get the new version via `brew upgrade xfade-dev/xfade/xfade`.

> The tap repo's remote must be HTTPS (`https://github.com/xfade-dev/homebrew-xfade.git`), not SSH — `brew update` fetches taps without credentials, and an SSH remote fails silently, leaving users pinned to whatever version the tap had when first cloned.
