# Packaging & release

## Versioning

- Semantic versioning `MAJOR.MINOR.PATCH`; crate versions are unified under `[workspace.package] version` in the root `Cargo.toml`.
- Before release, sync: `workspace.package.version` + `crates/gui/src-tauri/tauri.conf.json` `version`.

## CI

- `.github/workflows/ci.yml`: push/PR runs fmt + clippy + cargo test + frontend tsc + vitest.
- `.github/workflows/release.yml`: triggered by `git tag vX.Y.Z`:
  - Multi-platform CLI binaries (macOS arm/intel, linux, windows) + sha256.
  - Best-effort macOS GUI `.dmg`/`.app` (including the xfade sidecar). GUI
    packaging does not block signed CLI security releases until Developer ID
    signing and notarization are configured.
  - Aggregated into a GitHub Release + `SHA256SUMS`.
  - `SHA256SUMS` is signed keylessly with Sigstore; the bundle is published as
    `SHA256SUMS.sigstore.json`.
  - Every release asset receives a GitHub build-provenance attestation.
  - Release-path GitHub Actions are pinned to immutable commit SHAs.

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
2. In the tap repo `homebrew-xfade` (local clone, e.g. `/tmp/homebrew-xfade`, HTTPS read / deploy-key write), update `Formula/xfade.rb`: `version` + the two `sha256`. Also sync the copy in this repo's `homebrew/Formula/`.
3. `git commit -am "vX.Y.Z" && git push`; users get it via `brew upgrade xfade-dev/xfade/xfade`.
4. **Post-release verification (do not skip)**: `curl -fsSL https://xfade.sh | sh` into a temp dir and check the version; then `brew update && brew info xfade-dev/xfade/xfade` shows the new version.

## One-line installer (`xfade.sh`)

`scripts/install.sh` downloads the latest `xfade` CLI for the current macOS/Linux
platform. Point the `xfade.sh` domain (Cloudflare) at its raw URL:

```
https://raw.githubusercontent.com/xfade-dev/xfade/main/scripts/install.sh
```

Then users can run `curl -fsSL https://xfade.sh | sh`. Pin a version with
`XFADE_VERSION=vX.Y.Z`. The asset names it downloads match the release workflow
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

### CLI release artifacts

Release artifacts are signed by the GitHub Actions release workflow using
Sigstore keyless signing. The ephemeral certificate is bound to this exact
workflow identity and recorded in Sigstore's transparency log; no long-lived
private signing key is stored in GitHub.

Verify a release (requires `cosign`):

```bash
scripts/verify-release.sh vX.Y.Z
```

Or verify already-downloaded files directly:

```bash
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity \
    "https://github.com/xfade-dev/xfade/.github/workflows/release.yml@refs/tags/vX.Y.Z" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com"

sha256sum -c SHA256SUMS       # Linux
# shasum -a 256 -c SHA256SUMS # macOS
```

GitHub also stores build provenance for each asset:

```bash
gh attestation verify xfade-<target>.tar.gz --repo xfade-dev/xfade
```

The one-line installer verifies `SHA256SUMS` but does not install Cosign as a
dependency. Users who need signer identity and provenance verification should
run the command above before executing the downloaded binary.

### macOS GUI

The `.dmg` checksum is covered by the signed manifest and build provenance, but
the app is still ad-hoc code-signed. Gatekeeper distribution additionally
requires Apple Developer ID signing and notarization; configure Tauri's
`signingIdentity` and notarization credentials separately.

## crates.io

Only Cargo.toml metadata is set (publish-ready); nothing has been `cargo publish`ed yet. When needed:

```bash
cargo publish -p xfade-core   # publish core first (CLI/GUI depend on it)
cargo publish -p xfade              # bin name = xfade
```

(Verify the crate name is available before publishing; `xfade` may already be taken.)
