# Architecture

## Workspace structure

```
xfade/
├── crates/
│   ├── core/              # core library xfade-core
│   │   └── src/
│   │       ├── models.rs          # Provider / ToolKind / ToolKind::as_str
│   │       ├── store/
│   │       │   ├── db.rs          # rusqlite: providers / proxy_state / request_logs / stats
│   │       │   └── secrets.rs     # SecretStore trait + KeyringStore / MockStore / FileMockStore
│   │       ├── adapters/
│   │       │   ├── claude_code.rs # writes ~/.claude/settings.json (env.ANTHROPIC_*)
│   │       │   ├── codex.rs       # writes ~/.codex/config.toml + auth.json
│   │       │   └── opencode.rs    # writes ~/.config/opencode/opencode.json + auth.json
│   │       ├── presets.rs         # built-in presets (official / local-proxy)
│   │       ├── backup.rs          # pre-switch backup (nanosecond timestamp, keep 10)
│   │       ├── service.rs         # Core: main entry for config/switch/import/backup
│   │       ├── proxy/
│   │       │   ├── mod.rs         # ProxyService + build_router + serve/serve_with_shutdown
│   │       │   ├── forward.rs     # forwarding + failover + circuit breaking
│   │       │   ├── convert.rs     # Anthropic↔OpenAI protocol conversion
│   │       │   ├── usage.rs       # SSE tail-buffer token-usage extraction
│   │       │   ├── status.rs      # GET /__xfade/status + StatusDto/CircuitDto
│   │       │   ├── auth.rs        # auth_guard middleware
│   │       │   └── testsupport.rs # MockUpstream + test_core
│   │       ├── daemon.rs          # DaemonConfig + daemon.json read/write + launchctl load/unload
│   │       └── error.rs           # CoreError (thiserror, Send+Sync)
│   ├── cli/               # xfade binary
│   │   └── src/{main.rs, daemon.rs}   # clap commands + launchd plist writing
│   └── gui/               # Tauri app
│       ├── src/                   # React + TS frontend (Vite + Tailwind)
│       │   ├── pages/             # ProvidersPage / ProxyPage / StatsPage / LogsPage
│       │   ├── components/        # Layout / Toast / ProviderForm / RoutesEditor / BackupsPanel
│       │   └── api.ts             # invoke() wrappers
│       └── src-tauri/             # Rust backend
│           ├── src/{lib,commands,daemon_ctl,dto,state,tray}.rs
│           ├── tauri.conf.json    # bundle + externalBin (xfade sidecar)
│           └── binaries/xfade-<triple>   # build artifact (gitignored)
└── docs/
```

## Data model

- `Provider { id, tool, base_url, key_ref, extra, is_active }`, with `(tool, id)` composite primary key.
- `proxy_state`: `routes` (primary→fallback provider id list), `model_override`, `target_protocol`.
- `request_logs`: ts/endpoint/model/provider_id/status/tokens/duration/error.
- `stats_since(since, group_by)`: aggregates requests/tokens/errors/avg_duration.

`Core: Clone` (`Arc<dyn SecretStore>`), so the GUI and `ProxyService` share the same instance.

## Data flow

### CLI provider switch

```
xfade use <id> --tool <t>
  → Core::use_provider(tool, id)
    → ensure_imported → db.get(provider) → keyring.get(key_ref)
    → backup (back up config before writing)
    → adapter.apply(provider, key)   # write tool config files
    → db.set_active(tool, id)
```

### Proxy forwarding

```
POST /v1/chat/completions
  → auth_guard (auth_token check, if set)
  → forward::handle
    → db.get_routes() → routes[0]  # re-read every request; route changes take effect immediately
    → circuit_open? skip if tripped
    → convert (convert request body per target_protocol)
    → reqwest upstream (streaming passthrough)
    → record_success on success / record_failure on failure (≥3 → trip 60s)
    → usage extraction + db.insert_request_log
    → on failure, fail over to routes[i+1]
```

### Protocol conversion

`target_protocol` decides the request/response conversion direction: client protocol ↔ upstream protocol. tool_use/tool_calls arguments are aggregated and re-chunked as `input_json_delta` (streaming). See `proxy/convert.rs`.

### daemon (macOS)

```
xfade serve install
  → cli::daemon::install(home, host, port, token, load=true)
    → write ~/Library/LaunchAgents/ai.xfade.serve.plist (ProgramArguments=xfade serve --foreground …)
    → write $XFADE_DATA_DIR/daemon.json {host,port,auth_token}
    → launchctl unload + load (idempotent)

launchd-managed process = `xfade serve --foreground` → ProxyService::serve (axum)
  GET /__xfade/status → read db routes + in-memory circuits → JSON
```

### GUI ↔ daemon

```
GUI Proxy page "Start"
  → command proxy_start(app, host, port, token)
    → daemon_ctl::start(app, …)
      → app.shell().sidecar("xfade").args(["serve","install",…]).status()   # sidecar self-installs
    → poll daemon_ctl::status_from_config()  # HTTP GET /__xfade/status (with daemon.json token)
```

`xfade` is bundled into the .app as a Tauri sidecar (`bundle.externalBin`), so the GUI started from Finder doesn't need `~/.cargo/bin` on PATH. The sidecar's `current_exe()` = the in-app xfade, so the plist reference is correct.

## Testing

- core: `MockUpstream` (local mock upstream) + `test_core(tempdir, providers)` + `FileMockStore`, covering conversion/forwarding/circuit/status-endpoint/daemon file-level.
- CLI: `assert_cmd` integration tests + daemon file-level (no actual launchctl load).
- GUI: pure-function unit tests for commands/daemon_ctl + vitest frontend logic.
- Real launchctl load / GUI window smoke testing is left manual.
