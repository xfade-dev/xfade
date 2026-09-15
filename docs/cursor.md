# Cursor integration

Cursor (closed-source) exposes a standard **Override OpenAI Base URL** toggle, so `xfade`
can sit under it as a transparent local gateway — keep the virtual model selected and
hot-switch the underlying provider without restarting Cursor.

## 1. Point Cursor at the local proxy

1. Start the proxy (foreground, or as a resident daemon):

   ```bash
   xfade serve install --port 24860    # macOS daemon; or `xfade serve --port 24860` (foreground)
   ```

2. In Cursor **Settings → Models**, enable **Override OpenAI Base URL**:
   - **Base URL**: `http://127.0.0.1:24860/v1`
   - **API Key**: `xfade-local` (arbitrary; the proxy replaces it with the routed provider's key)

3. Set the model to a virtual name (e.g. `xfade`) or any name the proxy passes through.

## 2. Hot-switch the upstream

```bash
xfade proxy use kimi,bak --target-protocol chat     # primary → fallback, no restart
xfade proxy status
curl http://127.0.0.1:24860/__xfade/status          # routes + circuit status
```

Routes are re-read on every request, so the switch is immediate. `--target-protocol chat`
keeps the OpenAI wire format Cursor already speaks; `messages` targets Anthropic-native
upstreams (the proxy translates in both directions).

## 3. Model mapping

Cursor sends a model name; the proxy passes it through unless you override it per-route:

```bash
xfade proxy use kimi --model gpt-4o   # force the upstream model regardless of Cursor's setting
```

## Notes

- The proxy exposes `/v1/chat/completions`, `/v1/responses`, `/v1/messages`, and `/v1/models`
  (see [api.md](api.md)).
- The `local-proxy` preset points any OpenAI-compatible tool at the gateway in one command:
  `xfade use local-proxy --tool codex`, etc.
