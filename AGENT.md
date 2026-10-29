# AGENT.md

Guidance for AI agents working on this repository.

## What this is

xfade is a terminal AI-context control plane: it switches API providers between
AI coding tools (Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider / Cline /
Hermes Agent / OpenClaw) and exposes a local proxy with OpenAI/Anthropic-compatible
endpoints, failover routing, protocol conversion, and a resident daemon.

Rust workspace, single binary (`xfade` CLI) + optional Tauri GUI.

## Layout

```
crates/
  core/            # library: models, adapters, proxy, store, service, run
  cli/             # xfade binary: command dispatch + TUI
  gui/src-tauri/   # Tauri desktop app (auxiliary, not the core narrative)
docs/              # architecture, getting-started, api, release-notes
scripts/install.sh # one-line installer (served via xfade.sh)
homebrew/          # formula template (mirrored to the standalone tap repo)
```

## Build / test / lint

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # rustup toolchain may not be on PATH
cargo build --release -p xfade         # binary at target/release/xfade
cargo test                             # full test suite
cargo clippy --all-targets --workspace -- -D warnings
cargo fmt --all
```

GUI: `bash crates/gui/build-sidecar.sh` first, then `cd crates/gui && npx tauri dev`.

## Conventions

- **Adapters** implement `ToolAdapter` (`crates/core/src/adapters/`): `apply`,
  `read_current`, `capture_original_state`. A switch must back up the tool's
  config first and, on switching back to official, restore the captured original.
  Config writes go through `atomic_write` (temp + rename) + a `backups/` dir.
- **Two provider-env paths must stay in sync**: `apply` (writes the tool's config
  file) and `run_env` (`xfade run`, process env). The claude contract is shared via
  `claude_env_pairs` in `adapters/claude_code.rs`; if you change one, change both.
- **Tests first** for new behavior (TDD). Adapters use `tempfile::tempdir()` +
  a fake HOME; see existing adapter tests for the pattern.
- **Headless shells** fail the keyring backend (`User interaction is not allowed`);
  test with `xfade config set secrets file` (or `XFADE_SECRETS=file`).
- `cargo fmt` and clippy must be clean before a change is done.

## Docs

`docs/architecture.md`, `docs/getting-started.md`, `docs/api.md`, `CHANGELOG.md`,
`RELEASE.md` (release + homebrew-tap flow).
