# xfade

English | [简体中文](README.zh-CN.md)

Switch API providers between AI coding tools like Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider, with a local proxy (OpenAI/Anthropic-compatible endpoints, failover, protocol conversion) and a macOS resident daemon.

CLI (`xfade`) + Tauri GUI, implemented in Rust.

## Features

- **Provider switching**: add/remove/edit providers for Claude Code / Codex / OpenCode / Pi / Oh My Pi / Aider, one-command switch writes each tool's config (settings.json / config.toml+auth.json / opencode.json / .aider.conf.yml), stores keys in the system keyring, auto-backs-up before switching.
- **Local proxy**: `xfade serve` exposes OpenAI/Anthropic-compatible endpoints (`/v1/chat/completions`, `/v1/messages`, `/v1/responses`, `/v1/models`) with route failover, circuit breaking, token stats, and request logs.
- **Protocol conversion**: bidirectional Anthropic↔OpenAI conversion (`target_protocol`), decoupling clients from the upstream protocol.
- **daemon (macOS)**: launchd-resident + boot autostart + crash restart; `/__xfade/status` admin endpoint.
- **GUI**: Tauri desktop app, five pages (Providers / Proxy / Stats / Logs / Settings) + system tray (close-to-tray, quick switch, start/stop proxy); `xfade` self-installs the daemon as a sidecar.

## Installation

### CLI

```bash
# Homebrew (recommended, macOS)
brew tap xfade-dev/homebrew-xfade
brew install xfade

# or the one-line installer (macOS / Linux)
curl -fsSL https://xfade.sh | sh

# or download a binary from GitHub Release
# or build from source
cargo build --release -p xfade   # output target/release/xfade
```

### GUI

Download `xfade-gui_<version>_aarch64.dmg` (macOS Apple Silicon) from GitHub Release and drag it into Applications. Or build from source — see "Development" below.

## Quick start

```bash
# 1. Add a third-party provider (Codex as an example)
xfade add kimi --tool codex --base-url https://api.moonshot.cn/v1 --key sk-xxx

# 2. Switch to it (writes ~/.codex/config.toml + auth.json, auto-backs-up the old config)
xfade use kimi --tool codex

# 3. Show the current provider
xfade current --tool codex
```

Same for the other tools: `--tool claude-code|codex|open-code|pi|oh-my-pi|aider`.

## CLI command reference

```
xfade add <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
xfade ls [--tool <t>]
xfade use <id> --tool <t>
xfade current --tool <t>
xfade edit <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
xfade rm <id> --tool <t>
xfade presets --tool <t>
xfade import --tool <t>            # import from the tool's existing config
xfade backup ls|restore --tool <t>
xfade completion <shell>
```

### Local proxy

```bash
xfade serve [--port 24860] [--host 127.0.0.1] [--auth-token T]   # run in foreground
xfade proxy use --routes yy,bak [--model-override gpt-x] [--target-protocol chat|messages]
xfade proxy status
xfade proxy clear
xfade stats                  # aggregate by provider/model
```

### daemon (macOS)

```bash
xfade serve install [--port --host --auth-token]   # install as launchd-resident + boot autostart
xfade serve status                                 # query run state / routes / circuits
xfade serve uninstall                              # uninstall
```

Logs: `~/.config/xfade/serve.log`.

## GUI

```bash
cd crates/gui && npx tauri dev    # development
```

Left-sidebar pages:

- **Providers**: CRUD / switch / import / backups.
- **Proxy**: control the daemon (start/stop), edit route failover, circuit status.
- **Stats**: 24h/7d/30d × provider/model aggregation.
- **Logs**: the latest 200 request logs.
- **Settings**: global config (secrets backend, shared base_url/model/api, global API key).

System tray: close-to-tray, start/stop proxy, per-tool quick switch, quit. GUI autostarts on boot. The GUI embeds the `xfade` sidecar, so it can self-run `serve install` (no CLI pre-install needed).

## Environment contract

- `HOME`: tool config root.
- `XFADE_DATA_DIR`: self data dir (db/backups/daemon.json/config.json), defaults to `$HOME/.config/xfade`.

### Secret storage backend

The secret backend applies to **all tools** and is resolved with this priority:

1. `XFADE_SECRETS` env (`file` | `keyring`)
2. `XFADE_MOCK_SECRETS=1` (alias for `file`, tests/back-compat)
3. `<data_dir>/config.json` `secrets` field
4. default: system keyring

To persist the choice (no env var needed, applies to every tool):

```bash
xfade config                 # view current backend
xfade config set secrets file     # → ~/.config/xfade/config.json {"secrets":"file"}
xfade config set secrets keyring  # back to the system keychain
```

`secrets: file` stores API keys in `secrets.json` (owner-only 0600), which avoids the
macOS keychain authorization prompt that an ad-hoc-signed dev build triggers on every
rebuild. `keyring` uses the system keychain (recommended for signed release builds).

### Shared "general" config (all tools)

`config.json` also holds shared defaults — like CC Switch's general config — so you
set `base_url` / `model` / `api_key` / `api` once and every tool reuses them:

```bash
xfade config set base_url http://your-gateway:3000
xfade config set model glm-5-2-260617
xfade config set api anthropic-messages  # Pi/OMP wire protocol (optional)
xfade config set api_key sk-xxx        # stored in the secret backend, not config.json
xfade config                            # view all (api_key is masked)

xfade add glm --tool claude             # no --base-url/--key/--set → inherits the above
xfade use glm --tool claude
```

When `add` omits `--base-url` (non-interactive), it falls back to the global `base_url`
(use `--official` to force official login instead). An omitted `--key` falls back to the
global `api_key`; an omitted model falls back to the global `model`. A provider's
explicit values always win over the globals.

`api` selects Pi/OMP's wire protocol (`openai-completions` default, or
`anthropic-messages`). This matters for gateways like new-api/BM TokenHub whose OpenAI
streaming emits a trailing empty `choices` chunk that breaks `finish_reason` parsing —
switching to `anthropic-messages` sidesteps it.

CLI and GUI share the same data dir and backend, and can be used at the same time.

## Development

Rust workspace: `crates/core` (library) + `crates/cli` (`xfade`) + `crates/gui/src-tauri` (Tauri).

```bash
cargo test                       # full test suite
cargo clippy --all-targets --workspace -- -D warnings
cd crates/gui && npm install && npx tauri dev   # GUI dev (build the sidecar first)
bash crates/gui/build-sidecar.sh               # build the xfade sidecar into binaries/
```

See [docs/architecture.md](docs/architecture.md), [docs/getting-started.md](docs/getting-started.md), [docs/api.md](docs/api.md), [CHANGELOG.md](CHANGELOG.md), [RELEASE.md](RELEASE.md).

## License

MIT OR Apache-2.0
