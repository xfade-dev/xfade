# Getting started (per tool)

This guide demonstrates adding a third-party provider, switching, and the optional proxy mode for each supported tool. Prerequisite: install `xfade` (see README).

## Common flow

```bash
xfade add <id> --tool <tool> --base-url <url> --key <key> [--set '<extra json>']
xfade use <id> --tool <tool>
xfade current --tool <tool>
```

`<tool>` ∈ `claude-code | codex | open-code | pi | oh-my-pi | aider`. The original config is auto-backed-up before switching (`xfade backup ls --tool <tool>` to view/restore).

---

## Claude Code

Config file: `~/.claude/settings.json`, `env.ANTHROPIC_BASE_URL` + `env.ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`).

```bash
xfade add kimi --tool claude-code \
  --base-url https://api.moonshot.cn/anthropic \
  --key sk-xxx
xfade use kimi --tool claude-code
```

After switching, `settings.json`'s `env` gets the corresponding values, and the key is stored in the system keyring (`xfade/claude-code/kimi`). Official provider: `xfade use official --tool claude-code` (clears base_url, restores official).

## Codex

Config files: `~/.codex/config.toml` (`model_providers.<id>`) + `~/.codex/auth.json` (`OPENAI_API_KEY`).

```bash
xfade add kimi --tool codex --base-url https://api.moonshot.cn/v1 --key sk-xxx
xfade use kimi --tool codex
```

Notes:
- On apply, if `auth.json` contains official OAuth `tokens`, they are removed and `preferred_auth_method=apikey` is set, preventing the tokens from hijacking Authorization (backed up and recoverable).
- After apply, the whole file is scanned; if another provider section still uses the deprecated `wire_api="chat"`, a warning is emitted (newer Codex's full-file validation refuses to start; change it to `responses`).
- Official provider: `xfade use official --tool codex`.

## OpenCode

Config files: `~/.config/opencode/opencode.json` (`provider.<id>`) + `~/.local/share/opencode/auth.json`.

```bash
xfade add yy --tool open-code --base-url https://api.example.com/v1 --key sk-xxx
xfade use yy --tool open-code
```

## Pi

Config files: `~/.pi/agent/settings.json` (`defaultProvider`) + `~/.pi/agent/models.json` (the `xfade` provider entry).

```bash
xfade add openrouter --tool pi --base-url https://openrouter.ai/api/v1 --key sk-xxx --set model=gpt-4o
xfade use openrouter --tool pi
```

xfade writes a `xfade` provider into `models.json`, points `defaultProvider` to it, and restores the original `defaultProvider` when switching back to `official`. `--set api=anthropic-messages` selects the Pi wire protocol (`openai-completions` is the default).

## Oh My Pi (OMP)

Config files: `~/.omp/agent/settings.json` (`defaultProvider`) + `~/.omp/agent/models.yml` (the `xfade` provider entry).

```bash
xfade add deepseek --tool oh-my-pi --base-url https://api.deepseek.com --key sk-xxx --set model=deepseek-chat
xfade use deepseek --tool oh-my-pi
```

Same `defaultProvider` restore semantics as Pi; `--set api=...` selects the wire protocol.

## Aider

Config file: `~/.aider.conf.yml` (`model`, `openai-api-key`, `openai-api-base`).

```bash
xfade add kimi --tool aider --base-url https://api.moonshot.cn/v1 --key sk-xxx --set model=kimi-k2.5
xfade use kimi --tool aider
```

The model is written with an `openai/` prefix (Aider's litellm router requires it); the base URL is normalized to end with `/v1`. Switching back to `official` clears the three fields, leaving the rest of `.aider.conf.yml` untouched. Streaming defaults to off (avoids new-api gateways' empty-`choices` chunk); opt in with `--set stream=true`.

## Global config

Shared defaults (like CC Switch's general config), applied to every tool when a provider omits its own value:

```bash
xfade config                       # view (api_key masked)
xfade config set base_url http://your-gateway:3000
xfade config set model glm-5-2-260617
xfade config set api anthropic-messages   # Pi/OMP wire protocol
xfade config set api_key sk-xxx           # stored in the secret backend
xfade config set secrets file             # or keyring (system keychain)
```

A provider's explicit values always win over the globals. See the "Shared general config" section in README.

## Proxy mode (optional)

Instead of the tool connecting directly, `xfade serve` exposes a local compatible endpoint that the tool points at:

```bash
# 1. Start the proxy (foreground or daemon)
xfade serve install --port 24860        # macOS resident daemon
# or foreground: xfade serve --port 24860

# 2. Configure routes (primary→fallback)
xfade proxy use --routes yy,bak --model-override gpt-5.6-luna --target-protocol chat

# 3. Point the tool at the local proxy (Codex as an example)
xfade add local-proxy --tool codex --base-url http://127.0.0.1:24860/v1 --key any
xfade use local-proxy --tool codex

# 4. Inspect
xfade serve status          # run state / routes / circuits
xfade stats                 # token stats
curl http://127.0.0.1:24860/__xfade/status
```

`target_protocol`: `chat` (OpenAI style) / `messages` (Anthropic style), deciding the protocol-conversion direction. `model_override` passes through or overrides the upstream model name.

## GUI

`npx tauri dev` (or install the .dmg) → the Providers page works like the CLI above; the Proxy page starts/stops the daemon and edits routes; Stats/Logs show stats and logs; the tray provides quick switching.
