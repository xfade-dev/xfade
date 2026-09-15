# API reference

## `GET /__xfade/status`

Queries the proxy daemon run state, route config, and per-provider circuit status.

- **Auth**: if the daemon set an `auth_token`, `Authorization: Bearer <token>` is required; otherwise it's open. `/health` is auth-free.
- **Response** `200`:

```json
{
  "running": true,
  "host": "127.0.0.1",
  "port": 24860,
  "auth_enabled": true,
  "routes": ["yy", "bak"],
  "model_override": "gpt-5.6-luna",
  "target_protocol": "chat",
  "circuits": [
    { "provider_id": "yy", "fails": 2, "cooldown_remaining_secs": null },
    { "provider_id": "bak", "fails": 3, "cooldown_remaining_secs": 58 }
  ]
}
```

Fields:
- `running`: always true (the endpoint being alive means it's running).
- `circuits[].cooldown_remaining_secs`: remaining cooldown seconds; `null` means not tripped or already expired.
- `routes`: primary→fallback order; `model_override` null means passthrough.

```bash
curl http://127.0.0.1:24860/__xfade/status
curl -H "Authorization: Bearer <token>" http://127.0.0.1:24860/__xfade/status
```

## Proxy endpoints

| Method | Path | Description |
|---|---|---|
| GET | `/health` | liveness probe (auth-free), returns `ok` |
| POST | `/v1/chat/completions` | OpenAI Chat style |
| POST | `/v1/responses` | OpenAI Responses style |
| POST | `/v1/messages` | Anthropic Messages style |
| GET | `/v1/models` | model-list passthrough |
| GET | `/__xfade/status` | admin endpoint (see above) |

`/v1/*` goes through `auth_guard` (if auth_token is set) + route failover + protocol conversion (per `target_protocol`). Streaming responses are SSE-passthrough, with an 8KB tail buffer extracting usage for logging.

## CLI command reference

```
xfade add <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
xfade ls [--tool <t>]
xfade use <id> --tool <t>
xfade current
xfade edit <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
xfade rm <id> --tool <t>
xfade presets
xfade import --tool <t>
xfade backup ls --tool <t>
xfade backup restore --tool <t> <file>
xfade completion <shell>

xfade config                       # view global config (secrets backend, base_url/model/api, masked api_key)
xfade config set <key> <value>     # key: secrets|base_url|model|api|api_key

xfade serve [--port 24860] [--host 127.0.0.1] [--auth-token T]            # foreground
xfade serve install [--port --host --auth-token]                          # macOS daemon install
xfade serve uninstall | stop                                              # uninstall
xfade serve status                                                        # query status

xfade proxy use --routes a,b [--model-override M] [--target-protocol chat|messages]
xfade proxy status
xfade proxy clear
xfade stats
```

`--set '<json>'`: merged into the provider's `extra` field (e.g. Codex's `wire_api`, OpenCode's `models`).

`<tool>` ∈ `claude-code | codex | open-code | pi | oh-my-pi | aider`. The global config (`config.json`) holds the shared defaults (`base_url` / `model` / `api` / `api_key` / `secrets` backend) that a provider inherits when it omits its own value.

## GUI invoke commands

The frontend calls these via `@tauri-apps/api` `invoke` (Rust `tauri::command`):

| command | args |
|---|---|
| `list_providers` | `tool?` |
| `add_provider` / `update_provider` | `input { id, tool, base_url, key, extra }` |
| `remove_provider` / `use_provider` / `current_provider` | `tool, id?` |
| `list_presets` | `tool` |
| `import_provider` | `tool` |
| `list_backups` / `restore_backup` | `tool, path?` |
| `get_config` / `set_config` | `input { secrets?, base_url?, model?, api?, api_key? }` |
| `proxy_start` | `host, port, authToken` |
| `proxy_stop` / `proxy_status` | — |
| `get_routes` / `set_routes` / `clear_routes` | routes/modelOverride/targetProtocol |
| `stats` | `since, by` |
| `recent_logs` | `limit` |
