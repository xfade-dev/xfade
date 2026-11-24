# xfade

English | [简体中文](README.zh-CN.md)

**xfade** — the vendor-neutral, zero-raw-key gateway for AI coding CLIs (Claude Code, Codex, OpenCode, Aider, Cline, Hermes, OpenClaw, and more).

> **Switch models without dropping the beat** — First-class routing, protocol translation, robust failover, and dynamic pricing, with upstream API keys isolated completely from client configs. Zero telemetry, zero sponsored brokers, zero affiliate kickbacks.

[![CI](https://github.com/xfade-dev/xfade/actions/workflows/ci.yml/badge.svg)](https://github.com/xfade-dev/xfade/actions)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Security Policy](https://img.shields.io/badge/security-SLA%2048h%2F7d-green.svg)](SECURITY.md)

---

## Why xfade?

| Feature | Standard CLI Setup | Other Switchers / Proxies | xfade Gateway |
| :--- | :--- | :--- | :--- |
| **API Keys on Disk** | Raw plaintext in `settings.json` / `config.toml` | Raw plaintext in local configs / unencrypted DB | **Zero raw keys on disk** (client holds virtual `xf-tok-*`; real keys stay in OS Keyring) |
| **Failover Timing** | Hard error / session exit | Blind failover on any 4xx (wastes quota) | **Pre-commit failover** (TTFT timeout, 200 error body, 429 `Retry-After`, zero replay) |
| **Protocols** | Locked to client's native protocol | Rigid global translation | **Per-provider independent protocol** (Chat, Messages, Responses) |
| **Added Latency (F10)** | 0 ms | 5 ms – 50 ms (Python/Node runtime) | **~0.32 ms** (Rust Axum sub-millisecond overhead) |
| **Model Pricing** | Unknown / static tables | Outdated hardcoded rates | **Three-tier dynamic pricing** (Custom > OpenRouter live sync > 2026 flagship offline catalog) |
| **Trust & Neutrality** | N/A | Sponsored routes, telemetry, affiliate links | **100% Neutral**: zero telemetry, zero kickbacks, Sigstore signed, CycloneDX SBOM |

---

## Installation & Upgrade

### 1. Homebrew (macOS / Linux)

```bash
# Install
brew install xfade-dev/xfade/xfade

# Upgrade to latest release
brew update && brew upgrade xfade-dev/xfade/xfade
```

### 2. One-Line Installer (`xfade.sh`)

```bash
# Install or upgrade (default: ~/.local/bin)
curl -fsSL https://xfade.sh | sh

# Or install directly to system path:
curl -fsSL https://xfade.sh | XFADE_INSTALL_DIR=/usr/local/bin sh
```

### 3. Built-In Self-Update

```bash
xfade self-update
# (Use `sudo xfade self-update` if installed in /usr/local/bin)
```

### 4. Build from Source

```bash
cargo build --release -p xfade   # output in target/release/xfade
```

### Cryptographic Verification

Every release includes a keyless Sigstore signature and GitHub build provenance. Verify before running:

```bash
scripts/verify-release.sh v0.12.0
```

> [!TIP]
> **Troubleshooting Upgrades**: If `xfade --version` still shows an older version after reinstalling, run `which -a xfade` to check if an older binary in `/usr/local/bin` or `/opt/homebrew/bin` is shadowing `~/.local/bin`. Run `rehash` (Zsh) or `hash -r` (Bash) to refresh your shell's binary path cache.

---

## Quick Start: Zero-Raw-Key Isolation

### 1. Add Providers and Store Keys Securely

Add upstream providers. Real API keys are securely stored in your OS keychain (or encrypted local store):

```bash
# Add an Anthropic Messages provider
xfade add anthropic --tool claude --protocol messages \
  --base-url https://api.anthropic.com --key sk-ant-api03-...

# Add a DeepSeek or Kimi OpenAI-compatible Chat provider
xfade add kimi --tool claude --protocol chat \
  --base-url https://api.moonshot.cn/v1 --key sk-xxx
```

### 2. Switch with Credential Isolation (Default)

Running `xfade use` automatically provisions a virtual local token (`xf-tok-*`) and configures your coding tool to route through the local gateway:

```bash
xfade use kimi --tool claude
```

Your tool's configuration (`~/.claude/settings.json`) now points to `http://127.0.0.1:24860` with virtual token authorization. **No raw upstream key is ever written to disk.**

*(Need direct client-to-upstream communication without the gateway? Use the explicit `--direct` escape hatch: `xfade use kimi --tool claude --direct`).*

### 3. Verify Trust & Health

Run the built-in security doctor to check token isolation, file permissions, and trust status:

```bash
xfade doctor --security
```

If any configuration or file permission was modified externally, auto-remediate:

```bash
xfade doctor --security --fix
```

---

## Core Capabilities

### 1. Fine-Grained Failover Before First Byte (F6)
- **Pre-Commit Routing**: If an upstream returns an HTTP 200 with an internal JSON error body, or exceeds TTFT timeout, xfade transparently switches to the next configured fallback route.
- **Rate Limit Resilience**: Automatically respects upstream `Retry-After` headers (up to 2s) for instant intra-upstream retry before escalating.
- **Post-Commit Safety**: Once the first semantic chunk is emitted to the client, xfade **never replays** upstream data (preventing duplicated billing and duplicate tool executions). Interrupted streams cleanly emit a protocol-native `xfade_stream_interrupted` event.

### 2. Explicit Client-to-Upstream Model Mapping (F5)
Map general client model names to provider-specific model identifiers:

```bash
# Map client alias 'sonnet' to OpenRouter's full catalog name for the openrouter route
xfade model-map set openrouter sonnet anthropic/claude-3.7-sonnet

# List active model mappings
xfade model-map ls
```

### 3. Three-Tier Dynamic Model Pricing (F8 & F8-Ext)
Track token usage and estimated USD costs with micro-dollar integer precision:
- **Layer 1 (Custom Overrides)**: User-defined manual rates (`xfade pricing set`).
- **Layer 2 (Live Sync)**: Sync latest rates from OpenRouter's live catalog (`xfade pricing sync`).
- **Layer 3 (Offline Baseline)**: 2026 flagship catalog with prompt caching discounts (Claude 3.7/3.5, GPT-4o, DeepSeek V3/R1, Gemini 2.5).

```bash
xfade pricing list            # view pricing table & source (custom/synced/builtin)
xfade pricing sync            # pull live pricing from OpenRouter API
xfade stats --by model        # inspect token volume and estimated USD cost
```

### 4. Visible Audit Trail & Egress Security (F7)
- **SSRF & Data Exfiltration Protection**: Outbound requests are strictly limited to domains registered in your active provider configuration.
- **Audit Export**: Export sanitized request logs with full failover trajectories:
```bash
xfade audit export --format json
```
- **Trust Indicators**: TUI, CLI status, and desktop tray prominently display trust level:
  - `GUARDED`: Full zero-raw-key gateway isolation active.
  - `DIRECT`: Running in escape hatch mode (raw key on disk).
  - `EXPOSED`: Key leakage or insecure file permissions detected.

### 5. Verified Performance Benchmarks (F10)
Reproducible Criterion microbenchmarks run on local loopback:
- **Loopback Proxy Latency Overhead**: **~0.32 ms** (Direct: 0.032 ms vs Gateway: 0.353 ms)
- **Model Resolution Latency**: **~26 ns**
- **Micro-Dollar Pricing Lookup**: **~30 ns – 88 ns**
- **SSE Stream Chunk Semantic Inspection**: **< 1 µs**

---

## Command Reference

```bash
# Provider Management
xfade add <id> --tool <t> [--protocol chat|messages|responses] [--base-url URL] [--key KEY]
xfade use <id> --tool <t> [--direct]      # switch provider (default: zero-raw-key gateway)
xfade ls [--tool <t>]                     # list configured providers & active state
xfade current                             # show active provider for each tool

# Security, Audit & Health
xfade doctor --security [--fix]           # audit token isolation and file permissions
xfade audit [export]                      # view structured query audit trajectories
xfade status                              # print proxy daemon, routes, circuits, and trust state

# Model Mapping & Pricing
xfade model-map [set|ls|rm|clear]         # configure client-to-upstream model mappings
xfade pricing [list|sync|set|rm|reset]    # manage three-tier cost estimation catalog

# Proxy & Daemon Control
xfade serve [--port 24860] [--host 127.0.0.1]
xfade proxy use <p1> <p2> [--model M]     # setup failover cascade
xfade proxy smart on|off                  # Smart Fader intent routing
xfade stats [--by provider|model|session] # token and USD cost statistics
xfade serve install|uninstall             # launchd / systemd background service
xfade serve restart                        # restart the daemon (pick up a new build)
```

---

## Neutrality & Privacy Pledge

1. **Zero Raw Keys on Disk**: We do not store your plain upstream API keys in client config files or application logs.
2. **Zero Telemetry**: xfade makes no unsolicited analytics or tracking requests.
3. **Zero Sponsored Brokers & Zero Kickbacks**: We do not promote third-party proxy brokers, add attribution headers, or take affiliate cuts on model calls.
4. **Independent Verification**: Releases are signed with Sigstore keyless signing and verifiable via GitHub Build Provenance.

---

## Security

Please report vulnerabilities privately via GitHub Security Advisories or email **`security@xfade.dev`**.
Review our [Security Policy & SLA](SECURITY.md) for vulnerability response timelines.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
