# Packaging & release

## Versioning

- Semantic versioning `MAJOR.MINOR.PATCH`; crate versions are unified under `[workspace.package] version` in the root `Cargo.toml`.
- Before release, sync: `workspace.package.version` + `crates/gui/src-tauri/tauri.conf.json` `version`.

## CI

- `.github/workflows/ci.yml`: push/PR runs fmt + clippy + cargo test + frontend tsc + vitest.
- `.github/workflows/release.yml`: triggered by `git tag vX.Y.Z`:
  - Multi-platform CLI binaries (macOS arm/intel, linux, windows) + sha256.
  - macOS GUI `.dmg`/`.app` (including the xfade sidecar).
  - Aggregated into a GitHub Release + `SHA256SUMS`.

## Release flow

```bash
# 1. Confirm the version is updated (workspace.package.version + tauri.conf.json version)
# 2. Commit and tag
git commit -am "release: v0.7.0"
git tag v0.7.0
git push origin main --tags
# 3. The release workflow auto-builds and publishes the GitHub Release
```

## Post-release: update the Homebrew formula

1. Download `SHA256SUMS` from the GitHub Release and take the two macOS sha256 values.
2. In the tap repo `homebrew-xfade`, update `Formula/xfade.rb`: `version` + the two `sha256`.
3. `git commit -am "v0.7.0" && git push`; users get it via `brew upgrade xfade`.

## One-line installer (`xfade.sh`)

`scripts/install.sh` downloads the latest `xfade` CLI for the current macOS/Linux
platform. Point the `xfade.sh` domain (Cloudflare) at its raw URL:

```
https://raw.githubusercontent.com/xfade-dev/xfade/main/scripts/install.sh
```

Then users can run `curl -fsSL https://xfade.sh | sh`. Pin a version with
`XFADE_VERSION=v0.7.0`. The asset names it downloads match the release workflow
(`xfade-<arch>-<os>.tar.gz`).

## GUI sidecar (before local dev/build)

Before `cargo tauri dev`/`build`, tauri-build validates that `externalBin` (`binaries/xfade-<triple>`) exists. Build the sidecar locally first:

```bash
bash crates/gui/build-sidecar.sh   # cargo build --release -p xfade + copy to binaries/xfade-<host-triple>
```

The CI release workflow already includes an equivalent step; no manual action needed.

## Local GUI packaging (smoke)

```bash
cd crates/gui
bash build-sidecar.sh
npx tauri build        # produces src-tauri/target/release/bundle/{dmg,macos}/
```

## Signing

- Currently ad-hoc signed (runs locally on macOS, but Gatekeeper warns on distribution). Proper distribution requires Developer ID signing + notarization — configure `tauri.conf.json`'s `signingIdentity` + notarization later.
- Homebrew users are unaffected by Gatekeeper (binaries installed via brew).

## crates.io

Only Cargo.toml metadata is set (publish-ready); nothing has been `cargo publish`ed yet. When needed:

```bash
cargo publish -p xfade-core   # publish core first (CLI/GUI depend on it)
cargo publish -p xfade              # bin name = xfade
```

(Verify the crate name is available before publishing; `xfade` may already be taken.)
