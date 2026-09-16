# Design decisions

This file records the key architecture decisions and their rationale, extracted from the internal design specs. It replaces the per-version spec/plan documents (which are not committed).

## Protocol conversion (v0.3)

| Decision | Choice | Rationale |
|---|---|---|
| Conversion direction | Bidirectional (Anthropic↔OpenAI) | Request-side Anthropic→OpenAI + response-side OpenAI→Anthropic (streaming + non-stream) |
| Upstream OpenAI protocol | `chat/completions` | Most universal among OpenAI-compatible providers |
| tool_use streaming | Aggregate-then-rechunk | Aggregate split `arguments` fragments by index, then emit the full JSON as one `input_json_delta`; reliable, and Anthropic SDKs accept a full JSON string |
| thinking/reasoning blocks | Dropped | Not converted |
| Model mapping | Route-level `model_override` | `xfade proxy use --model <openai-model>` |
| Conversion trigger | inbound `messages` + `target_protocol=chat` | `target_protocol=messages` keeps v0.2 passthrough |
| Error responses | Passed through verbatim | Not converted to Anthropic error format |

## Proxy & failover (v0.2)

| Decision | Choice |
|---|---|
| Circuit breaker | `FAIL_THRESHOLD=3`, `COOLDOWN=60s` |
| Usage extraction (streaming) | 8KB tail ring buffer, parsed at stream end |
| Route read | Re-read from DB on every request (route changes take effect immediately) |
| Streaming usage injection | `stream_options.include_usage=true` injected for chat streaming |

## Daemon & GUI (v0.4–v0.5)

| Decision | Choice |
|---|---|
| Daemon manager | launchd (macOS) + systemd (Linux) |
| GUI framework | Tauri v2 (React + TS + Vite + Tailwind) |
| Daemon self-install | `xfade` bundled as a Tauri sidecar (`bundle.externalBin`), solving the Finder PATH issue |
| Proxy embedding | The proxy runs in the same process as the GUI |

## Keyring migration (v0.6)

| Decision | Choice |
|---|---|
| Service name + key_ref prefix | `agent-switch` → `xfade` |
| Migration strategy | Lazy, on `Core` startup; idempotent; best-effort |
| Atomicity | Migrate secret first, update DB only on success (avoid permanently skipping a failed provider) |

## Explicit non-goals (deferred / boundaries)

- True incremental tool_use streaming (aggregate-then-rechunk is sufficient for agentic use).
- thinking/reasoning block conversion (dropped).
- Multimodal image optimization (data URI passthrough only).
- Per-provider `target_protocol` (the whole route shares one).
- Error-response format conversion (upstream errors passed through).
- GUI autostart toggle (spec'd, not yet implemented).
- Tray dynamic status + 5s refresh (spec'd, not yet implemented).
- Apple Developer ID signing + notarization (ad-hoc signing for now; Homebrew install is unaffected by Gatekeeper).
