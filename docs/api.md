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
| GET | `/v1/models` | model-list passthrough (first success); aggregated across all routes when `models-merge` is on |
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

xfade proxy use a b [--model M] [--route-model <id>=<model>] [--target chat|messages]
xfade proxy status
xfade proxy clear
xfade proxy smart on|off
xfade proxy failover-4xx on|off
xfade proxy models-merge on|off
xfade stats
```

`--set '<json>'`: merged into the provider's `extra` field (e.g. Codex's `wire_api`, OpenCode's `models`).

`<tool>` ∈ `claude-code | codex | open-code | pi | oh-my-pi | aider`. The global config (`config.json`) holds the shared defaults (`base_url` / `model` / `api` / `api_key` / `secrets` backend) that a provider inherits when it omits its own value.

## Per-provider model overrides & 4xx failover

`--model M` sets a single global upstream model applied to every route. When providers expose disjoint model catalogs (e.g. a local gateway vs. a cloud router), use repeatable `--route-model <id>=<model>` to pin a model per provider; `--model` stays the fallback for any route without an explicit entry.

```bash
xfade proxy use freebuff omni \
  --route-model freebuff=z-ai/glm-5.3-flash \
  --route-model omni=auto/best-fast \
  --target chat
xfade proxy status        # prints the per-route model map
```

`xfade proxy failover-4xx on|off` (default **off**) additionally fails over on narrow 4xx responses: `401`, `403`, and `400` model-rejection responses (`model_not_allowed`, "model … not in / not allowed / not found"). Transport errors, `429`, and `5xx` always fail over regardless of this toggle; generic `400`/`404`/`422` still pass through to the client.

## Aggregated model catalog (`models-merge`)

By default `GET /v1/models` is a plain failover forward, so a client only ever sees the **first** successful upstream's catalog. When several routes expose disjoint model lists, that hides most of them from AI coding tools.

`xfade proxy models-merge on` (default **off**) changes `/v1/models` into an aggregator: the daemon probes `/v1/models` on every configured route concurrently (5s timeout each, skipping circuit-open providers) and merges the `data[]` arrays into one `{"object":"list","data":[...]}` response.

```bash
xfade proxy use freebuff omni --target chat
xfade proxy models-merge on
xfade proxy status           # prints "models merge: on"
curl -s http://127.0.0.1:24860/v1/models | jq '.data[].id'
```

- **Model IDs stay raw** when an id is exposed by only one route (the common case). When two routes expose the *same* id, those entries are exposed as `<route_id>/<model>` (e.g. `omni/gpt-x`) so the id is unambiguous.
- The daemon persists a `model → owning route` map (rebuilt on each aggregated `/v1/models`). A later POST carrying one of those ids is **pinned to the owning provider first** and the forwarded body uses the real upstream model id (a namespaced id is stripped). All other routes remain as failover with their existing `--route-model` / `--model` overrides.
- A client model that is **not** in the map (a legacy hardcoded id, or none at all) keeps 100% of the pre-feature behavior: `--route-model` → global `--model` → Smart Fader / static ordering.
- The toggle and the map are read per request, so `models-merge on|off` and a fresh `GET /v1/models` take effect on a running daemon with no restart. `proxy use` / `proxy clear` invalidate the map; it is rebuilt on the next `/v1/models` call.
- If **no** upstream returns a usable catalog, `/v1/models` degrades to the original first-success forward rather than returning an empty list. The last-known map is retained.
- Turning the toggle **off** restores the exact original first-success behavior on both GET and POST.

`xfade proxy status` prints the `models merge` state; `/__xfade/status` exposes it as `models_merge`.

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
