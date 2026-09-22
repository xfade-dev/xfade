# Changelog

This project follows [Semantic Versioning](https://semver.org/).

## v0.8.2 — 2026-09-22

### New
- **feat(core)**: Hermes Agent (`hermes`) adapter — writes a `model:` block (provider: custom / default / base_url / api_key) to `~/.hermes/config.yaml`; switching back to official restores the captured OAuth `model:` block.
- **feat(core)**: OpenClaw (`openclaw`) adapter — writes a custom provider under `models.providers.xfade` (baseUrl / apiKey / api / models[]) and pins `agents.defaults.model.primary` to `xfade/<model>`; reads JSON5 configs (comments, trailing commas, unquoted keys); switching back to official removes the provider and restores the captured primary.
- **feat(presets)**: OpenRouter / Kimi / DeepSeek presets + local proxy (`http://127.0.0.1:24860/v1`) for Hermes and OpenClaw.
- **feat(gui)**: Cline (previously missing) and Hermes / OpenClaw added to the provider-management UI.
- **feat(self-update)**: hint `sudo xfade self-update` when the Homebrew-installed binary can't be replaced (root-owned `/opt/homebrew/bin`).

## v0.8.1 — 2026-09-21

### Fixes
- **fix(codex)**: third-party providers now write `requires_openai_auth = true` — Codex ≥0.155 treats a provider with neither `env_key` nor `requires_openai_auth` as unauthenticated and drops the Authorization header (upstream 401 "No api key passed in").
- **fix(daemon)**: `xfade serve install` no longer deletes the plist it just wrote (the old `unload()` removed the file, so `load()` failed with "daemon not installed"); `serve status` now prints the host.

### New
- **feat(opencode)**: accept `--set model=<id>` shorthand (synthesizes the `models` object) alongside the verbose `--set 'models={...}'` form.

## v0.8.0 — 2026-09-21

### New
- **feat(cli)**: crossfader TUI is now the default facade — bare `xfade` opens it (per-tool tabs, provider health badges, parallel probe-all, inline edit). Non-TTY environments degrade to a plain list, so scripts stay safe. Previously bare `xfade` printed help.
- **feat(cli)**: Claude Code per-slot models — `--opus-model` / `--sonnet-model` / `--haiku-model` on `add`/`edit` (written as `ANTHROPIC_DEFAULT_*_MODEL`); rejected on non-Claude tools instead of being silently ignored.

### Fixes & refactor
- **feat(core)**: integration fixes across the seven supported clients + core audit refactor.

### Docs
- **docs**: README (EN/zh-CN) now documents the TUI, `xfade self-update`, and `xfade config`; fixed the `presets` reference (the `--tool` flag never existed).

## v0.7.0 — 2026-09-14

### New tools
- **feat(core)**: Pi / Oh My Pi (`pi`, `omp`) + Aider (`aider`) adapters (6 tools total).
- **feat(core)**: shared "general" config (`config.json`): secrets backend, global `base_url`/`model`/`api`, and a global API key; CLI `xfade config [set]`.
- **feat(core)**: `Core::update_config` (persist global config; empty value clears the field).

### GUI
- **feat(gui)**: Settings page (secrets backend + shared base_url/model/api + global API key).
- **feat(gui)**: Providers page now lists all 6 tools (was 3); `ToolKind` type covers pi/oh-my-pi/aider.

### Packaging
- **feat**: `scripts/install.sh` one-line installer (`curl -fsSL https://xfade.sh | sh`, macOS/Linux).

### Docs
- **docs**: `docs/cursor.md` (Cursor takeover); `getting-started.md` / `api.md` updated for the new tools + global config.

## v0.6.0 — 2026-07-29

### Tech debt
- **fix(core)**: codex apply removes official OAuth `tokens` + sets `preferred_auth_method=apikey`, preventing Authorization hijacking.
- **fix(core)**: stats errors now count `status=0` (upstream connection failures).
- **feat(core)**: codex apply scans the whole file for deprecated `wire_api="chat"` and warns.

### Packaging & release
- **chore**: workspace `[workspace.package]` metadata (v0.6.0, license MIT OR Apache-2.0).
- **ci**: CI workflow (fmt/clippy/test + frontend tsc/vitest); release workflow (tag-triggered multi-platform CLI binaries + macOS GUI .dmg + checksums + GitHub Release).
- **feat(gui)**: `xfade` bundled into the .app as a Tauri sidecar; the GUI self-installs the daemon via `tauri-plugin-shell` (solves the Finder PATH issue).
- **feat**: Homebrew formula (standalone tap `homebrew-xfade`).
- `core::daemon::install` is idempotent (unload-then-load).
- `RELEASE.md` packaging/release guide.

### Docs
- README restructure; added `docs/architecture.md`, `docs/getting-started.md`, `docs/api.md`, `CHANGELOG.md`.

## v0.5.0 — 2026-07-28

- **feat(core)**: `ProxyService` gains `host/port` fields + `with_host_port` builder.
- **refactor(core)**: extracted a shared `auth_guard` middleware (`/health` is auth-free, `/v1/*` and `/__xfade/status` share the check).
- **feat(core)**: `GET /__xfade/status` admin endpoint + `CircuitDto` moved down; `core::daemon` (DaemonConfig + daemon.json + launchctl load/unload).
- **feat(cli)**: `xfade serve install/uninstall/status/stop` (launchd-resident + RunAtLoad + KeepAlive).
- **feat(gui)**: `daemon_ctl` replaces the embedded `proxy_ctl` (HTTP status query + launchctl control); system tray (close-to-tray + per-tool quick switch + start/stop proxy) + `tauri-plugin-autostart` boot autostart.

## v0.4.0 — 2026-07-28

- **feat(core)**: `Core: Clone` (`Arc<dyn SecretStore>`); `serve_with_shutdown` graceful shutdown; serde derive for `RequestLog/StatsRow/StatsGroupBy/Preset`.
- **feat(gui)**: Tauri v2 desktop GUI (React + TS + Vite + Tailwind), four pages Providers/Proxy/Stats/Logs; 18 Tauri commands calling Core directly; the proxy is embedded in-process.

## v0.3.0 — 2026-07-27

- **feat(core)**: Anthropic→OpenAI protocol conversion (tool_calls aggregated and re-chunked as `input_json_delta`).
- `proxy_state` gains `model_override`, `target_protocol` (chat/messages).

## v0.2.0 — 2026-07-26

- **feat**: local proxy `xfade serve` (axum + reqwest + SSE streaming passthrough).
- Route failover + circuit breaking (FAIL_THRESHOLD=3, COOLDOWN=60s).
- `xfade proxy use/status/clear` + `xfade stats`; `request_logs` + `proxy_state` tables.

## v0.1.0 — 2026-07-24

- **feat**: provider-switching CLI (add/ls/use/current/edit/rm/presets/import/backup/completion).
- Three adapters (claude_code/codex/opencode) + system-keyring key storage + backup rotation before switching.
- `xfade-core` library (rusqlite Mutex/Send+Sync).
