# Packaging & Release Guide

This document defines the release standard operating procedure (SOP), distribution channels, verification protocols, and maintenance workflows for `xfade`.

---

## 1. Versioning & Pre-Release Checklist

We strictly adhere to [Semantic Versioning](https://semver.org/) (`MAJOR.MINOR.PATCH`). All crates in the workspace inherit `[workspace.package] version`.

Before tagging a release, complete this checklist:

1. **Root Manifest**: Update `version = "X.Y.Z"` in `Cargo.toml`.
2. **GUI Manifest**: Update `"version": "X.Y.Z"` in `crates/gui/src-tauri/tauri.conf.json`.
3. **Changelog**: Add a new `## vX.Y.Z — YYYY-MM-DD` section in `CHANGELOG.md`.
4. **Standalone Release Notes**: Create `docs/release-notes/vX.Y.Z.md`.
5. **Quality & Security Gates**:
   ```bash
   cargo fmt --check
   cargo clippy --workspace --all-targets --exclude xfade-gui -- -D warnings
   cargo test --workspace --exclude xfade-gui
   cargo deny check
   ```

---

## 2. Release Execution & CI Automation

Triggering the release is done via Git tag:

```bash
# 1. Commit synchronized manifests and documentation
git commit -am "release: vX.Y.Z"

# 2. Create annotated tag and push
git tag -a vX.Y.Z -m "release: vX.Y.Z"
git push origin main
git push origin vX.Y.Z
```

### GitHub Actions Workflow (`.github/workflows/release.yml`)
Pushing a tag `v*` initiates the automated release pipeline:
- **Multi-Platform Matrix Build**: Builds release CLI binaries for:
  - `aarch64-apple-darwin` (macOS Apple Silicon)
  - `x86_64-apple-darwin` (macOS Intel)
  - `x86_64-unknown-linux-gnu` (Linux x86_64)
  - `x86_64-pc-windows-msvc` (Windows x86_64)
- **macOS GUI**: Builds `.dmg` / `.app` bundle with embedded CLI sidecar.
- **CycloneDX SBOM**: Generates CycloneDX 1.5 SBOM (`xfade.cdx.json`) limited to crates in the CLI build graph.
- **SHA256 Manifest**: Aggregates all release assets into `SHA256SUMS`.
- **Sigstore Keyless Signing**: Keylessly signs `SHA256SUMS` using GitHub OIDC; writes `SHA256SUMS.sigstore.json`.
- **Build Provenance**: Generates GitHub provenance attestations for all release artifacts.
- **GitHub Release**: Publishes the official GitHub release with generated release notes.

---

## 3. Post-Release: Sync All Distribution Channels

Once GitHub Actions finishes publishing the release (typically 4–6 minutes after tag push), update all distribution channels:

### 3.1 Homebrew Formula & Tap Repository (Mandatory)

Run the dedicated automation script to sync the standalone tap (`xfade-dev/homebrew-xfade`):

```bash
bash scripts/update-homebrew-formula.sh vX.Y.Z
```

**What this script does:**
1. Downloads `SHA256SUMS` from `https://github.com/xfade-dev/xfade/releases/download/vX.Y.Z/SHA256SUMS`.
2. Extracts SHA256 hashes for both macOS ARM64 and Intel tarballs.
3. Updates `homebrew/Formula/xfade.rb` in the local repository.
4. Clones `git@github.com:xfade-dev/homebrew-xfade.git`, updates its formula, commits with `vX.Y.Z`, and pushes to GitHub.

**Verification:**
```bash
brew update && brew info xfade-dev/xfade/xfade
# Output must show the new version
```

### 3.2 One-Line Installer (`https://xfade.sh`)

`scripts/install.sh` automatically resolves the newest release via GitHub's `releases/latest/download` URL.

**Verification:**
```bash
# Test installation into a temporary directory
curl -fsSL https://xfade.sh | XFADE_INSTALL_DIR=/tmp/xfade-test sh
/tmp/xfade-test/xfade --version
rm -rf /tmp/xfade-test
```

### 3.3 Built-In Self-Update (`xfade self-update`)

The CLI's `self-update` command queries GitHub's latest release API, verifies SHA256, and atomically replaces the binary.

**Verification:**
```bash
xfade self-update
```

### 3.4 Cryptographic Verification (Sigstore & Provenance)

Verify the release signatures and build provenance (requires `cosign`):

```bash
scripts/verify-release.sh vX.Y.Z
```

Or verify manually:
```bash
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "https://github.com/xfade-dev/xfade/.github/workflows/release.yml@refs/tags/vX.Y.Z" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com"
```

---

## 4. Troubleshooting: Upgrades & Reinstalls Showing Old Versions

If a user or tester reports that reinstalling still shows the previous version, check the following:

### Issue A: Multiple Binaries in PATH (Shadowing)
Different installation methods place the binary in different directories:
- **Homebrew**: `/opt/homebrew/bin/xfade` (macOS ARM) or `/usr/local/bin/xfade` (macOS Intel / Linux)
- **xfade.sh**: `$HOME/.local/bin/xfade` (fallback: `/usr/local/bin/xfade`)
- **Cargo**: `$HOME/.cargo/bin/xfade`

If `$HOME/.local/bin` comes *after* `/usr/local/bin` or `/opt/homebrew/bin` in `$PATH`, running `xfade` invokes the older binary.

**Fix**:
```bash
# 1. Identify all installed xfade instances
which -a xfade

# 2. Overwrite the specific target directory:
curl -fsSL https://xfade.sh | XFADE_INSTALL_DIR=/opt/homebrew/bin sh
# or:
curl -fsSL https://xfade.sh | XFADE_INSTALL_DIR=/usr/local/bin sh
```

### Issue B: Shell Path Hash Cache
Bash and Zsh cache binary paths in memory for fast lookup. After replacing a binary in-place, the shell may still point to the old location or state.

**Fix**:
```bash
# For Zsh (macOS default):
rehash

# For Bash:
hash -r
```

### Issue C: Homebrew Tap Out of Sync
If `brew upgrade` reports "already up-to-date" with an older version, Homebrew's local index is stale.

**Fix**:
```bash
brew update
brew upgrade xfade-dev/xfade/xfade
```

### Issue D: Daemon Still Running the Old Build After Upgrade
Upgrading (via `brew upgrade`, `xfade self-update`, or `install.sh`) only replaces the binary on disk. A running `xfade serve` daemon keeps the old build in memory, so fixes in the new release do not take effect until the daemon is restarted. **The daemon does not auto-restart.**

**Fix**:
```bash
xfade serve restart
# equivalent on macOS:
launchctl kickstart -k gui/$(id -u)/ai.xfade.serve
```

`xfade doctor` reports a daemon/CLI version mismatch, and `xfade use` (gateway mode) now auto-restarts a stale daemon via the version handshake.

---

## 5. Summary Release Checklist

| Step | Action | Command | Done |
| :---: | :--- | :--- | :---: |
| 1 | Bump `Cargo.toml` & `tauri.conf.json` | Edit version strings | [ ] |
| 2 | Update `CHANGELOG.md` & `docs/release-notes/` | Add release notes | [ ] |
| 3 | Run tests & clippy | `cargo test && cargo clippy` | [ ] |
| 4 | Git commit & tag push | `git tag -a vX.Y.Z && git push --tags` | [ ] |
| 5 | Wait for GitHub Actions build | Check GitHub Actions tab | [ ] |
| 6 | **Sync Homebrew Tap** | `bash scripts/update-homebrew-formula.sh vX.Y.Z` | [ ] |
| 7 | Verify `xfade.sh` install | `curl -fsSL https://xfade.sh \| sh` | [ ] |
| 8 | Verify `xfade self-update` | `xfade self-update` | [ ] |
| 9 | Verify Sigstore signature | `scripts/verify-release.sh vX.Y.Z` | [ ] |
