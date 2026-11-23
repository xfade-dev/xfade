use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;
use xfade_core::proxy::circuit_breaker::CircuitState;
use xfade_core::proxy::ProxyService;
use xfade_core::store::db::StatsGroupBy;
use xfade_core::{
    presets::presets_for, AddProviderInput, Config, Core, CoreError, Provider, ProviderProtocol,
    ToolKind,
};

mod daemon;
mod tui;

const ROOT_HELP_TEMPLATE: &str = "\
{about-with-newline}
Usage:
  xfade <COMMAND> [OPTIONS]
  xfade <TOOL> [PROVIDER] [-- <ARGS>...]

CORE ROUTING & SWITCHING:
  use          Switch provider (default: local gateway; --direct: write to disk)
  run          Run a tool CLI with a provider injected via env vars (zero disk mutation)
  ls           List providers
  status       View current status (daemon, routes, circuit breaker, and trust status)
  current      Show the currently-active provider per tool
  tui          Interactive TUI for switching providers

GATEWAY & TRAFFIC GOVERNANCE:
  serve        Start the local proxy service / manage the daemon
  proxy        Proxy route management (primary/fallback provider order)
  model-map    Explicit model mappings across upstreams (alias: mm)
  pricing      Manage model pricing catalogs, custom overrides, and live sync
  stats        Request stats aggregation and cost estimation
  sessions     Unified session view: conversations seen by the local gateway

SECURITY & TRUST:
  doctor       Run security health check and credential hygiene audit (--fix)
  audit        View and export structured security audit logs
  egress       Manage outbound egress whitelist and SSRF guard policy
  handover     Generate seamless handover brief from recent session and git state

CONFIGURATION & ASSETS:
  add          Add a provider (omit NAME to enter interactive mode)
  edit         Edit a provider
  rm           Remove a provider (the active one cannot be removed)
  import       Manually import the tool's current config as the imported snapshot
  presets      List built-in presets
  config       View or set global config (e.g. secrets backend)
  backup       Backup management

TOOL LAUNCHERS (Zero-config instant run):
  claude       Launch Claude Code with a provider injected via env
  codex        Launch Codex — config overrides via `-c` + key via env
  opencode     Launch OpenCode — inline OPENCODE_CONFIG_CONTENT
  aider        Launch Aider with a provider injected via env
  pi, omp      Launch Pi / Oh My Pi
  cline        Launch Cline
  hermes       Launch Hermes Agent
  openclaw     Launch OpenClaw

GENERAL:
  self-update  Update xfade to the latest GitHub release
  completion   Generate shell completions
  project      Show the analyzer's fingerprint of the current project

{options}
Use 'xfade <command> --help' for more information about a specific command.
";

#[derive(Parser)]
#[command(
    name = "xfade",
    version,
    about = "Vendor-neutral, zero-raw-key gateway for AI coding CLIs",
    help_template = ROOT_HELP_TEMPLATE
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add a provider (omit NAME to enter interactive mode)
    Add {
        /// Provider name, e.g. `xfade add kimi --tool claude ...`
        name: Option<String>,
        #[arg(long)]
        tool: Option<ToolKind>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        key: Option<String>,
        /// Tool-specific config, repeatable: --set model=kimi-k2.5
        #[arg(long = "set", value_parser = parse_kv)]
        sets: Vec<(String, String)>,
        /// Force official login (ignore the global base_url)
        #[arg(long)]
        official: bool,
        /// Upstream protocol format (chat, messages, or responses)
        #[arg(long)]
        protocol: Option<ProviderProtocol>,
        /// Claude Code only: model for the `opus` slot (ANTHROPIC_DEFAULT_OPUS_MODEL)
        #[arg(long)]
        opus_model: Option<String>,
        /// Claude Code only: model for the `sonnet` slot (ANTHROPIC_DEFAULT_SONNET_MODEL)
        #[arg(long)]
        sonnet_model: Option<String>,
        /// Claude Code only: model for the `haiku` slot (ANTHROPIC_DEFAULT_HAIKU_MODEL)
        #[arg(long)]
        haiku_model: Option<String>,
    },
    /// List providers
    Ls {
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// Switch provider
    Use {
        name: String,
        #[arg(long)]
        tool: Option<ToolKind>,
        /// Direct mode: write raw upstream API key into client config on disk instead of using the local gateway
        #[arg(long)]
        direct: bool,
    },
    /// Run a tool CLI with a provider injected via env vars (claude / aider only, no config changes; shorthand: `xfade claude <name>`)
    Run {
        /// Provider name (as shown in `xfade ls`)
        name: String,
        #[arg(long)]
        tool: Option<ToolKind>,
        /// Arguments after `--` are passed to the tool itself
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Show the currently-active provider per tool
    Current,
    /// View current status (daemon, routes, circuit breaker, and trust status)
    Status,
    /// Run security health check and credential hygiene audit
    Doctor {
        /// Run deep security inspection across client configurations, permissions, and gateway bindings
        #[arg(long, default_value_t = true)]
        security: bool,
        /// Automatically remediate file permissions and convert exposed configs to loopback gateway
        #[arg(long)]
        fix: bool,
    },
    /// View and export structured security audit logs
    Audit {
        /// Filter audit logs by client tool (e.g. claude, codex, opencode)
        #[arg(long)]
        tool: Option<ToolKind>,
        /// Number of recent records to display (default: 20)
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Export all audit logs instead of viewing recent table
        #[arg(long)]
        export: bool,
        /// Export format (json or csv, default: json)
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Manage outbound egress whitelist and SSRF guard policy
    Egress {
        #[command(subcommand)]
        cmd: Option<EgressCmd>,
    },
    /// Explicit model mappings across upstreams
    #[command(name = "model-map", alias = "mm")]
    ModelMap {
        #[command(subcommand)]
        cmd: ModelMapCmd,
    },
    /// Edit a provider
    Edit {
        name: String,
        #[arg(long)]
        tool: Option<ToolKind>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        key: Option<String>,
        #[arg(long = "set", value_parser = parse_kv)]
        sets: Vec<(String, String)>,
        /// Claude Code only: model for the `opus` slot (ANTHROPIC_DEFAULT_OPUS_MODEL)
        #[arg(long)]
        opus_model: Option<String>,
        /// Claude Code only: model for the `sonnet` slot (ANTHROPIC_DEFAULT_SONNET_MODEL)
        #[arg(long)]
        sonnet_model: Option<String>,
        /// Claude Code only: model for the `haiku` slot (ANTHROPIC_DEFAULT_HAIKU_MODEL)
        #[arg(long)]
        haiku_model: Option<String>,
    },
    /// Remove a provider (the active one cannot be removed)
    Rm {
        name: String,
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// List built-in presets
    Presets,
    /// View or set global config (e.g. secrets backend)
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },
    /// Manually import the tool's current config as the imported snapshot
    Import {
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// Backup management
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// Generate shell completions
    Completion { shell: Shell },
    /// Start the local proxy service (OpenAI/Anthropic-compatible endpoints) / manage the daemon
    Serve {
        #[arg(long, default_value = "24860")]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Require `Authorization: Bearer <token>` (or `x-api-key`). Visible in the
        /// process list; prefer --auth-token-file. Mandatory for non-loopback --host.
        #[arg(long)]
        auth_token: Option<String>,
        /// Read the auth token from a file (keeps it out of the process list)
        #[arg(long, conflicts_with = "auth_token")]
        auth_token_file: Option<PathBuf>,
        #[command(subcommand)]
        cmd: Option<ServeCmd>,
    },
    /// Proxy route management (primary/fallback provider order)
    Proxy {
        #[command(subcommand)]
        cmd: ProxyCmd,
    },
    /// Request stats aggregation
    Stats {
        #[arg(long, default_value = "7d")]
        since: String,
        #[arg(long, value_parser = ["provider", "model", "session"])]
        by: Option<String>,
    },
    /// Unified session view: conversations seen by the local gateway
    Sessions {
        #[command(subcommand)]
        cmd: Option<SessionsCmd>,
    },
    /// Show the analyzer's fingerprint of the current project (language / git pulse)
    Project,
    /// Update xfade to the latest GitHub release
    SelfUpdate,
    /// Generate a seamless handover brief from the recent session and git state
    Handover {
        /// Copy handover brief to system clipboard
        #[arg(long)]
        copy: bool,
        /// Only output the raw markdown brief (no formatting/hints)
        #[arg(long)]
        raw: bool,
    },
    /// Interactive TUI for switching providers
    Tui {
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// Manage model pricing catalogs, custom overrides, and live synchronization
    Pricing {
        #[command(subcommand)]
        cmd: Option<PricingCmd>,
    },
    /// Launch Claude Code with a provider injected via env (instant `run`)
    Claude(RunOpts),
    /// Launch Codex — config overrides via `-c` + key via env (instant `run`)
    Codex(RunOpts),
    /// Launch OpenCode — inline OPENCODE_CONFIG_CONTENT (instant `run`)
    #[command(name = "opencode")]
    OpenCode(RunOpts),
    /// Launch Pi — config-file tool, instant launch unsupported (see `xfade use`)
    Pi(RunOpts),
    /// Launch Oh My Pi — config-file tool, instant launch unsupported (see `xfade use`)
    #[command(name = "omp", alias = "oh-my-pi")]
    OhMyPi(RunOpts),
    /// Launch Aider with a provider injected via env (instant `run`)
    Aider(RunOpts),
    /// Launch Cline — config-file tool, instant launch unsupported (see `xfade use`)
    Cline(RunOpts),
    /// Launch Hermes Agent — config-file tool, instant launch unsupported (see `xfade use`)
    Hermes(RunOpts),
    /// Launch OpenClaw — config-file tool, instant launch unsupported (see `xfade use`)
    #[command(name = "openclaw")]
    OpenClaw(RunOpts),
}

/// Instant-launch options for the per-tool subcommands:
/// `xfade <tool> [name] [--model m] [--base-url u] [--key k] [-- <args>]`.
#[derive(clap::Args)]
struct RunOpts {
    /// Provider or preset name; omit to launch the tool's active provider
    name: Option<String>,
    /// One-off model override (resolved to a provider/preset when NAME is omitted)
    #[arg(long)]
    model: Option<String>,
    /// One-off endpoint override (flips an official provider to third-party)
    #[arg(long)]
    base_url: Option<String>,
    /// API key (stored key first, then the global key)
    #[arg(long)]
    key: Option<String>,
    /// Capture and copy handover context from the previous session into clipboard
    #[arg(long)]
    handover: bool,
    /// Arguments after `--` are passed to the tool itself
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Set a config value, e.g. `xfade config set secrets file`
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum ProxyCmd {
    /// Set proxy routes (primary → fallback, in argument order)
    Use {
        names: Vec<String>,
        /// Override the upstream model field (global fallback; passthrough the client's model when omitted)
        #[arg(long)]
        model: Option<String>,
        /// Per-provider model override, repeatable: --route-model <id>=<model>.
        /// Overrides --model for that provider; essential when upstreams have
        /// disjoint model catalogs.
        #[arg(long = "route-model", value_parser = parse_kv)]
        route_models: Vec<(String, String)>,
        /// Target protocol: chat (OpenAI-compatible) or messages (Anthropic native passthrough)
        #[arg(long, value_parser = ["chat", "messages"], default_value = "chat")]
        target: String,
    },
    /// View current routes
    Status,
    /// Clear routes
    Clear,
    /// Toggle the Smart Fader (intent-aware local/cloud tiered routing)
    Smart {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Toggle failover on qualifying 4xx responses (401/403 and narrow 400 model-rejection).
    /// Default off; 429/5xx/transport errors always fail over.
    #[command(name = "failover-4xx")]
    Failover4xx {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Aggregate /v1/models across all routes and pin posts by the client-selected model.
    /// Default off (first-success passthrough).
    #[command(name = "models-merge")]
    ModelsMerge {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Toggle transparent session-handover injection on the next proxy request (L3).
    /// Default off. When on, pending model-switch context is prepended to the
    /// last user message (never elevated into system/instructions).
    #[command(name = "handover-inject")]
    HandoverInject {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
}

#[derive(Subcommand)]
enum ServeCmd {
    /// Install as a launchd resident daemon (macOS, autostart on boot + restart on crash)
    Install {
        #[arg(long, default_value = "24860")]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long)]
        auth_token: Option<String>,
    },
    /// Uninstall the daemon
    Uninstall,
    /// Stop the daemon (alias of uninstall)
    Stop,
    /// Query daemon status
    Status,
}

#[derive(Subcommand)]
enum SessionsCmd {
    /// Show one session's details and its model-switch timeline
    Show { id: String },
}

#[derive(Subcommand)]
enum BackupCmd {
    Ls {
        #[arg(long)]
        tool: ToolKind,
    },
    Restore {
        #[arg(long)]
        tool: ToolKind,
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum EgressCmd {
    /// List current egress policy and permitted outbound domains
    List,
    /// Add a domain or host to the egress whitelist
    Add {
        /// Outbound domain to allow (e.g. api.example.com)
        domain: String,
    },
    /// Remove a domain from the egress whitelist
    Remove {
        /// Outbound domain to remove
        domain: String,
    },
    /// Set egress policy mode (registered or unrestricted)
    Policy {
        /// Mode: 'registered' (only registered provider endpoints and whitelist) or 'unrestricted'
        mode: String,
    },
}

#[derive(Subcommand)]
enum ModelMapCmd {
    /// Set mappings for a client model alias: xfade model-map set <alias> <provider>=<upstream_model>...
    Set {
        alias: String,
        /// Repeatable provider=model mappings (e.g. anthropic=claude-3-7-sonnet deepseek=deepseek-chat *=claude-3-7-sonnet)
        #[arg(required = true, value_parser = parse_kv)]
        mappings: Vec<(String, String)>,
        /// Allow mapping provider names not currently registered in xfade
        #[arg(long)]
        force: bool,
    },
    /// List all configured client model aliases and their upstream mappings
    #[command(alias = "list")]
    Ls,
    /// Remove a model mapping (omit provider to remove all mappings for the alias)
    Rm {
        alias: String,
        provider: Option<String>,
    },
    /// Clear all model mappings across all aliases
    Clear,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PricingCmd {
    /// List active model pricing across custom, synced, and built-in layers
    List {
        /// Optional keyword or model filter
        #[arg(long, short)]
        filter: Option<String>,
    },
    /// Synchronize model pricing catalog from OpenRouter or custom URL
    Sync {
        /// Custom JSON endpoint URL (defaults to OpenRouter public models API)
        #[arg(long)]
        url: Option<String>,
    },
    /// Set a custom model pricing override (rates in USD per 1M tokens)
    Set {
        /// Target model pattern or name
        model: String,
        /// Input prompt token price in USD per 1M tokens (e.g. 2.00)
        #[arg(long, allow_hyphen_values = true)]
        input: f64,
        /// Output completion token price in USD per 1M tokens (e.g. 10.00)
        #[arg(long, allow_hyphen_values = true)]
        output: f64,
        /// Cached input read token price in USD per 1M tokens (defaults to 10% of input)
        #[arg(long, allow_hyphen_values = true)]
        cache_read: Option<f64>,
        /// Cache creation/write token price in USD per 1M tokens (defaults to 125% of input)
        #[arg(long, allow_hyphen_values = true)]
        cache_write: Option<f64>,
    },
    /// Remove a custom model pricing override
    Rm {
        /// Model pattern to remove
        model: String,
    },
    /// Reset pricing table (e.g. clear synced cache)
    Reset {
        /// Clear synced pricing entries
        #[arg(long)]
        synced: bool,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s}"))
}

/// Validate and collect repeatable `--route-model <id>=<model>` values.
/// An id must be one of the configured route names; ids cannot repeat and
/// neither side may be empty.
fn build_route_models(
    names: &[String],
    raw: &[(String, String)],
) -> Result<std::collections::BTreeMap<String, String>, CoreError> {
    let known: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut out = std::collections::BTreeMap::new();
    for (id, model) in raw {
        if id.is_empty() || model.is_empty() {
            return Err(CoreError::ConfigParse {
                path: "route-model".into(),
                msg: format!("expected <id>=<model> with non-empty parts, got {id:?}={model:?}"),
            });
        }
        if !known.contains(id.as_str()) {
            return Err(CoreError::ConfigParse {
                path: "route-model".into(),
                msg: format!("unknown provider id '{id}' (must be one of the route names)"),
            });
        }
        if out.contains_key(id) {
            return Err(CoreError::ConfigParse {
                path: "route-model".into(),
                msg: format!("duplicate --route-model for provider '{id}'"),
            });
        }
        out.insert(id.clone(), model.clone());
    }
    Ok(out)
}

/// --set value: use JSON when it parses (objects/arrays/numbers/booleans), otherwise treat as a string
fn parse_set_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
}

/// Claude Code per-slot model flags (opus/sonnet/haiku). Grouped so the
/// add/edit commands don't grow a long, slot-flavored parameter list.
#[derive(Default)]
struct PerSlotModels {
    opus: Option<String>,
    sonnet: Option<String>,
    haiku: Option<String>,
}

impl PerSlotModels {
    /// Convert to `extra` entries. These flags only make sense for Claude Code;
    /// using one on another tool is a usage error rather than something to
    /// silently ignore.
    fn entries(&self, tool: ToolKind) -> Result<Vec<(String, serde_json::Value)>, CoreError> {
        let mut out = Vec::new();
        for (flag, val) in [
            ("opus-model", &self.opus),
            ("sonnet-model", &self.sonnet),
            ("haiku-model", &self.haiku),
        ] {
            if let Some(v) = val {
                if tool != ToolKind::ClaudeCode {
                    return Err(CoreError::ConfigParse {
                        path: "args".into(),
                        msg: format!("`--{flag}` is only valid for `--tool claude`"),
                    });
                }
                out.push((flag.replace('-', "_"), serde_json::Value::String(v.clone())));
            }
        }
        Ok(out)
    }
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn build_core() -> Result<Core, CoreError> {
    Core::from_env()
}

/// Resolve the self data dir (`XFADE_DATA_DIR`, defaulting to `~/.config/xfade`).
fn data_dir() -> Result<PathBuf, CoreError> {
    let home = std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
    Ok(std::env::var_os("XFADE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(&home).join(".config").join("xfade")))
}

fn run(cli: Cli) -> Result<(), CoreError> {
    let Some(cmd) = cli.cmd else {
        // Bare `xfade`: the TUI is the default facade (see docs/architecture).
        // run_tui degrades to a plain list when not on a terminal.
        let core = build_core()?;
        let tool = resolve_tui_tool(&core)?;
        return tui::run_tui(&core, tool);
    };
    match cmd {
        Cmd::Add {
            tool,
            name,
            base_url,
            key,
            protocol,
            sets,
            official,
            opus_model,
            sonnet_model,
            haiku_model,
        } => cmd_add(
            tool,
            name,
            base_url,
            key,
            protocol,
            sets,
            official,
            PerSlotModels {
                opus: opus_model,
                sonnet: sonnet_model,
                haiku: haiku_model,
            },
        ),
        Cmd::Ls { tool } => {
            let core = build_core()?;
            let list = core.list(tool)?;
            print_table(&list);
            if tool.is_none() {
                if let Ok(mappings) = core.db().get_model_mappings() {
                    if !mappings.is_empty() {
                        let aliases: Vec<String> = mappings.keys().cloned().collect();
                        println!("\nModel mappings configured: {}", aliases.join(", "));
                    }
                }
            }
            Ok(())
        }
        Cmd::Run { name, tool, args } => cmd_run(&name, tool, &args),
        Cmd::Claude(o) => cmd_run_instant(ToolKind::ClaudeCode, o),
        Cmd::Codex(o) => cmd_run_instant(ToolKind::Codex, o),
        Cmd::OpenCode(o) => cmd_run_instant(ToolKind::OpenCode, o),
        Cmd::Pi(o) => cmd_run_instant(ToolKind::Pi, o),
        Cmd::OhMyPi(o) => cmd_run_instant(ToolKind::OhMyPi, o),
        Cmd::Aider(o) => cmd_run_instant(ToolKind::Aider, o),
        Cmd::Cline(o) => cmd_run_instant(ToolKind::Cline, o),
        Cmd::Hermes(o) => cmd_run_instant(ToolKind::Hermes, o),
        Cmd::OpenClaw(o) => cmd_run_instant(ToolKind::OpenClaw, o),
        Cmd::Use { name, tool, direct } => {
            let core = build_core()?;
            let tool = resolve_tool(&core, &name, tool)?;
            // Special case: provider doesn't exist but matches a preset → auto-create from the preset.
            // - official: no key needed.
            // - local-proxy: key is arbitrary (the proxy replaces it with the routed provider's key).
            // - other third-party presets (kimi/glm/...): they need a real API key, so refuse to
            //   auto-create with a bogus placeholder — writing a placeholder into the tool config
            //   (e.g. ANTHROPIC_AUTH_TOKEN) breaks the tool's auth immediately.
            let exists = core.list(Some(tool))?.iter().any(|p| p.id == name);
            if !exists {
                if let Some(preset) = presets_for(tool).into_iter().find(|p| p.id == name) {
                    let is_local_proxy = name == "local-proxy";
                    if preset.is_official() || is_local_proxy {
                        let mut p = Provider::new(&name, tool, preset.base_url);
                        p.extra = preset.extra;
                        if is_local_proxy {
                            // The local-proxy preset carries no model, but OpenClaw
                            // (and Hermes) bake the model into their config. Fall back
                            // to the global default model, mirroring `add`.
                            let mut map = p.extra.as_object().cloned().unwrap_or_default();
                            if !map.contains_key("model") {
                                if let Some(m) = &core.config().model {
                                    map.insert(
                                        "model".to_string(),
                                        serde_json::Value::String(m.clone()),
                                    );
                                }
                            }
                            if !map.is_empty() {
                                p.extra = serde_json::Value::Object(map);
                            }
                            // OpenClaw requires a model to set `agents.defaults.model.primary`;
                            // without one the switch would fail and leave a half-added provider.
                            if tool == ToolKind::OpenClaw
                                && p.extra.get("model").and_then(|m| m.as_str()).is_none()
                            {
                                return Err(CoreError::ConfigParse {
                                    path: "local-proxy".into(),
                                    msg: format!(
                                        "OpenClaw needs a model to route through the local proxy.\n\
                                         First add it with a model:\n\
                                         \x20 xfade add local-proxy --tool openclaw --base-url {} --key x --set model=<model>\n\
                                         (or set a global default: xfade config set model <model>)\n\
                                         Then switch:\n\
                                         \x20 xfade use local-proxy --tool openclaw",
                                        p.base_url
                                            .as_deref()
                                            .unwrap_or("http://127.0.0.1:24860/v1")
                                    ),
                                });
                            }
                        }
                        let key = if is_local_proxy {
                            Some("xfade-local-proxy".to_string())
                        } else {
                            None
                        };
                        core.add_provider(p, key.as_deref())?;
                        println!("auto-created {tool}/{name} from preset");
                    } else {
                        return Err(CoreError::MissingApiKey(format!(
                            "'{name}' is a third-party preset and needs an API key.\n\
                             First add it with a key:\n\
                             \x20 xfade add {name} --tool {tool} --base-url {} --key <sk-...>\n\
                             Then switch:\n\
                             \x20 xfade use {name} --tool {tool}",
                            preset.base_url.as_deref().unwrap_or("")
                        )));
                    }
                }
            }
            core.use_provider(tool, &name, direct)?;
            println!("switched {tool} -> {name}");
            Ok(())
        }
        Cmd::Current => {
            let core = build_core()?;
            for t in ToolKind::ALL {
                match core.current(t)? {
                    Some(p) => println!(
                        "{t}: {} ({})",
                        p.id,
                        p.base_url.as_deref().unwrap_or("official")
                    ),
                    None => println!("{t}: (none)"),
                }
            }
            Ok(())
        }
        Cmd::Status => {
            let core = build_core()?;
            cmd_status(&core)?;
            Ok(())
        }
        Cmd::Doctor { security: _, fix } => {
            let core = build_core()?;
            cmd_doctor(&core, fix)?;
            Ok(())
        }
        Cmd::Audit {
            tool,
            limit,
            export,
            format,
        } => {
            let core = build_core()?;
            cmd_audit(&core, tool, limit, export, &format)?;
            Ok(())
        }
        Cmd::Egress { cmd } => {
            let core = build_core()?;
            cmd_egress(&core, cmd)?;
            Ok(())
        }
        Cmd::ModelMap { cmd } => {
            let core = build_core()?;
            cmd_model_map(&core, cmd)?;
            Ok(())
        }
        Cmd::Rm { name, tool } => {
            let core = build_core()?;
            let tool = resolve_tool(&core, &name, tool)?;
            core.remove(tool, &name)?;
            println!("removed {tool}/{name}");
            Ok(())
        }
        Cmd::Presets => {
            for t in ToolKind::ALL {
                println!("[{t}]");
                for p in presets_for(t) {
                    println!(
                        "  {:<12} {:<20} {}",
                        p.id,
                        p.label,
                        p.base_url.unwrap_or_else(|| "(official)".to_string())
                    );
                }
            }
            Ok(())
        }
        Cmd::Config { cmd } => {
            match cmd {
                None => {
                    let dir = data_dir()?;
                    let cfg = Config::load(&dir);
                    println!("secrets: {}", cfg.secrets);
                    println!("base_url: {}", cfg.base_url.as_deref().unwrap_or("(unset)"));
                    println!("model: {}", cfg.model.as_deref().unwrap_or("(unset)"));
                    println!("api: {}", cfg.api.as_deref().unwrap_or("(unset)"));
                    let api_key = build_core()?.global_api_key();
                    match api_key {
                        Some(_) => println!("api_key: (set)"),
                        None => println!("api_key: (unset)"),
                    }
                    Ok(())
                }
                Some(ConfigCmd::Set { key, value }) => {
                    let core = build_core()?;
                    match key.as_str() {
                        "secrets" => {
                            core.update_config(Some(&value), None, None, None)?;
                        }
                        "base_url" => {
                            core.update_config(None, Some(&value), None, None)?;
                        }
                        "model" => {
                            core.update_config(None, None, Some(&value), None)?;
                        }
                        "api" => {
                            core.update_config(None, None, None, Some(&value))?;
                        }
                        "api_key" => {
                            core.set_global_api_key(&value)?;
                        }
                        other => {
                            return Err(CoreError::ConfigParse {
                            path: other.to_string(),
                            msg: "unknown config key (expected: secrets|base_url|model|api|api_key)".into(),
                        });
                        }
                    }
                    println!("set {key} = {value}");
                    Ok(())
                }
            }
        }
        Cmd::Edit {
            name,
            tool,
            base_url,
            key,
            sets,
            opus_model,
            sonnet_model,
            haiku_model,
        } => {
            let core = build_core()?;
            let tool = resolve_tool(&core, &name, tool)?;
            let mut p = core
                .list(Some(tool))?
                .into_iter()
                .find(|p| p.id == name)
                .ok_or_else(|| CoreError::ProviderNotFound(format!("{tool}/{name}")))?;
            if let Some(url) = base_url {
                p.base_url = Some(url);
            }
            let mut map = p.extra.as_object().cloned().unwrap_or_default();
            for (k, v) in sets {
                map.insert(k, parse_set_value(&v));
            }
            let slots = PerSlotModels {
                opus: opus_model,
                sonnet: sonnet_model,
                haiku: haiku_model,
            };
            for (k, v) in slots.entries(tool)? {
                map.insert(k, v);
            }
            if !map.is_empty() {
                p.extra = serde_json::Value::Object(map);
            }
            core.update_provider(&p, key.as_deref())?;
            println!("updated {tool}/{name}");
            Ok(())
        }
        Cmd::Import { tool } => {
            let core = build_core()?;
            let tools: Vec<ToolKind> = tool.map_or_else(|| ToolKind::ALL.to_vec(), |t| vec![t]);
            for t in tools {
                match core.import(t)? {
                    Some(p) => println!(
                        "imported {t} snapshot: {}",
                        p.base_url.as_deref().unwrap_or("(no base url in config)")
                    ),
                    None => println!("{t}: nothing to import"),
                }
            }
            Ok(())
        }
        Cmd::Backup { cmd } => match cmd {
            BackupCmd::Ls { tool } => {
                let core = build_core()?;
                for b in core.backups(tool)? {
                    println!("{}", b.display());
                }
                Ok(())
            }
            BackupCmd::Restore { tool, file } => {
                let core = build_core()?;
                core.restore_backup(tool, &file)?;
                println!("restored {}", file.display());
                Ok(())
            }
        },
        Cmd::Completion { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        Cmd::Serve {
            port,
            host,
            auth_token,
            auth_token_file,
            cmd,
        } => match cmd {
            None => {
                let auth_token = match auth_token_file {
                    Some(p) => {
                        let t = std::fs::read_to_string(&p).map_err(|e| {
                            CoreError::Proxy(format!("read --auth-token-file {}: {e}", p.display()))
                        })?;
                        Some(t.trim().to_string())
                    }
                    None => auth_token,
                };
                let core = build_core()?;
                // Warm the repo_maps cache with the daemon's cwd so
                // `xfade proxy status` can show the served project. Signal-
                // free directories are not cached (analyze() skips them).
                if let Ok(cwd) = std::env::current_dir() {
                    let map = xfade_core::analyzer::analyze(&cwd, core.db());
                    if map.lang.is_some() || map.git.is_some() {
                        eprintln!(
                            "[xfade] project: {} (lang {}, branch {})",
                            map.root,
                            map.lang.unwrap_or("?"),
                            map.git.as_ref().map(|g| g.branch.as_str()).unwrap_or("-")
                        );
                    }
                }
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|e| CoreError::Proxy(format!("create tokio runtime: {e}")))?;
                rt.block_on(async {
                    ProxyService::new(core)
                        .with_host_port(&host, port)
                        .with_auth_token(auth_token)
                        .serve()
                        .await
                })
            }
            Some(ServeCmd::Install {
                port,
                host,
                auth_token,
            }) => {
                let home =
                    std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
                daemon::install(std::path::Path::new(&home), &host, port, auth_token, true)?;
                println!(
                    "installed daemon (label {}); logs: ~/.config/xfade/serve.log",
                    xfade_core::daemon::LABEL
                );
                Ok(())
            }
            Some(ServeCmd::Uninstall) | Some(ServeCmd::Stop) => {
                let home =
                    std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
                daemon::uninstall(std::path::Path::new(&home), true)?;
                println!("uninstalled daemon");
                Ok(())
            }
            Some(ServeCmd::Status) => {
                let home =
                    std::env::var("HOME").map_err(|_| CoreError::Keyring("HOME not set".into()))?;
                let dd = xfade_core::daemon::data_dir(std::path::Path::new(&home));
                match xfade_core::daemon::read(&dd) {
                    None => {
                        println!("daemon: not installed");
                        Ok(())
                    }
                    Some(cfg) => {
                        let url = format!("http://{}:{}/__xfade/status", cfg.host, cfg.port);
                        let rt = tokio::runtime::Runtime::new()
                            .map_err(|e| CoreError::Proxy(format!("rt: {e}")))?;
                        rt.block_on(async {
                            let client = reqwest::Client::new();
                            let mut req = client.get(&url);
                            if let Some(t) = &cfg.auth_token {
                                req = req.header("Authorization", format!("Bearer {t}"));
                            }
                            match req.send().await {
                                Ok(r) if r.status().is_success() => {
                                    let v: serde_json::Value = r.json().await.unwrap_or_default();
                                    println!(
                                        "daemon: running {}:{} auth={}",
                                        cfg.host,
                                        cfg.port,
                                        cfg.auth_token.is_some()
                                    );
                                    println!("routes: {:?}", v["routes"]);
                                }
                                Ok(r) => println!("daemon: HTTP {}", r.status()),
                                Err(_) => println!(
                                    "daemon: not responding (check ~/.config/xfade/serve.log)"
                                ),
                            }
                        });
                        Ok(())
                    }
                }
            }
        },
        Cmd::Proxy { cmd } => match cmd {
            ProxyCmd::Use {
                names,
                model,
                route_models,
                target,
            } => {
                if names.is_empty() {
                    return Err(CoreError::ConfigParse {
                        path: String::new(),
                        msg: "no provider names given; usage: xfade proxy use <name> [<name>...]"
                            .into(),
                    });
                }
                let core = build_core()?;
                let known: std::collections::HashSet<String> =
                    core.list(None)?.into_iter().map(|p| p.id).collect();
                for n in &names {
                    if !known.contains(n) {
                        return Err(CoreError::ProviderNotFound(format!(
                            "{n} (not in any tool; run `xfade ls` to list)"
                        )));
                    }
                }
                let route_model_map = build_route_models(&names, &route_models)?;
                let prev = core.db().get_routes().ok().flatten();
                core.db().set_routes_with_models(
                    &names,
                    model.as_deref(),
                    &target,
                    &route_model_map,
                )?;
                // Session timeline: mid-flight model switches land here
                // (`proxy use --model`), not in `xfade use` — the gateway picks
                // them up on the very next request without a client restart.
                // The handover's session_id is backfilled by the proxy from
                // the next matching request. Best-effort.
                let from = prev
                    .as_ref()
                    .and_then(|(r, m, _)| m.clone().or_else(|| r.first().cloned()));
                let to = model.clone().or_else(|| names.first().cloned());
                if from != to {
                    let _ = xfade_core::sessions::record_handover(
                        core.db(),
                        "",
                        from.as_deref(),
                        to.as_deref(),
                    );
                }
                let main = &names[0];
                let backups: Vec<&str> = names.iter().skip(1).map(|s| s.as_str()).collect();
                if backups.is_empty() {
                    println!("proxy route: {main}");
                } else {
                    println!("proxy route: {main} -> {}", backups.join(" -> "));
                }
                match model.as_deref() {
                    Some(m) => println!("model: {m}"),
                    None => println!("model: (passthrough)"),
                }
                for (id, m) in &route_model_map {
                    println!("route model: {id} = {m}");
                }
                println!("target: {target}");
                Ok(())
            }
            ProxyCmd::Status => {
                let core = build_core()?;
                cmd_status(&core)?;
                Ok(())
            }
            ProxyCmd::Clear => {
                let core = build_core()?;
                core.db().clear_routes()?;
                println!("routes cleared");
                Ok(())
            }
            ProxyCmd::Smart { state } => {
                let core = build_core()?;
                let on = state == "on";
                core.db().set_smart_enabled(on)?;
                if on {
                    println!("smart fader: on — light intents (error triage, tests, checks, small refactors) go local-first; heavy intents (big refactors, architecture) go cloud-first");
                } else {
                    println!("smart fader: off — static route order");
                }
                Ok(())
            }
            ProxyCmd::Failover4xx { state } => {
                let core = build_core()?;
                let on = state == "on";
                core.db().set_failover_4xx(on)?;
                if on {
                    println!("failover 4xx: on — qualifying 401/403 and model-rejection 400 responses trigger failover");
                } else {
                    println!("failover 4xx: off — only 429/5xx/transport errors trigger failover");
                }
                Ok(())
            }
            ProxyCmd::ModelsMerge { state } => {
                let core = build_core()?;
                let on = state == "on";
                core.db().set_models_merge_enabled(on)?;
                if on {
                    println!("models merge: on — /v1/models aggregates all routes; posts are pinned to the provider owning the selected model");
                } else {
                    println!("models merge: off — /v1/models is first-success passthrough");
                }
                Ok(())
            }
            ProxyCmd::HandoverInject { state } => {
                let core = build_core()?;
                let on = state == "on";
                core.db().set_handover_auto_inject_enabled(on)?;
                if on {
                    println!("handover inject: on — pending model-switch context is prepended to the next request's last user message (opt-in L3)");
                } else {
                    println!(
                        "handover inject: off — proxy does not rewrite request bodies for handover"
                    );
                }
                Ok(())
            }
        },
        Cmd::Stats { since, by } => {
            let since_ts = parse_since(&since)?;
            let group_by = match by.as_deref() {
                Some("model") => StatsGroupBy::Model,
                Some("session") => StatsGroupBy::Session,
                _ => StatsGroupBy::Provider,
            };
            let core = build_core()?;
            let rows = core.db().stats_since(&since_ts, group_by, None)?;
            if rows.is_empty() {
                println!("no requests in the last {since}");
                return Ok(());
            }
            println!(
                "{:<24} {:>10} {:>10} {:>12} {:>8} {:>8}",
                "group", "requests", "tokens", "cost (est)", "errors", "avg_ms"
            );
            for r in &rows {
                let tokens = r.prompt_tokens + r.completion_tokens;
                let cost_str = xfade_core::proxy::pricing::format_cost(r.cost_usd_micros);
                println!(
                    "{:<24} {:>10} {:>10} {:>12} {:>8} {:>8}",
                    r.group, r.requests, tokens, cost_str, r.errors, r.avg_duration_ms
                );
            }
            println!("* cost estimates based on official token pricing (estimated)");
            Ok(())
        }
        Cmd::Sessions { cmd } => match cmd {
            None => {
                let core = build_core()?;
                let rows = core.db().list_sessions(50)?;
                if rows.is_empty() {
                    println!(
                        "no sessions yet (they appear once requests flow through `xfade serve`)"
                    );
                    return Ok(());
                }
                println!(
                    "{:<26} {:<8} {:<10} {:<16} {:>8} {:>12} {:<8} started",
                    "session", "tool", "provider", "model", "tokens", "cost (est)", "status"
                );
                for r in &rows {
                    let tokens = r.prompt_tokens + r.completion_tokens;
                    let cost_str = xfade_core::proxy::pricing::format_cost(r.cost_usd_micros);
                    println!(
                        "{:<26} {:<8} {:<10} {:<16} {:>8} {:>12} {:<8} {}",
                        r.id,
                        r.tool,
                        r.provider_id.as_deref().unwrap_or("-"),
                        r.model.as_deref().unwrap_or("-"),
                        tokens,
                        cost_str,
                        r.status,
                        r.started_at
                    );
                }
                Ok(())
            }
            Some(SessionsCmd::Show { id }) => {
                let core = build_core()?;
                let s = core
                    .db()
                    .get_session(&id)?
                    .ok_or_else(|| CoreError::ConfigParse {
                        path: "sessions".into(),
                        msg: format!("session '{id}' not found (see `xfade sessions`)"),
                    })?;
                println!("session:  {}", s.id);
                println!("tool:     {}", s.tool);
                println!("provider: {}", s.provider_id.as_deref().unwrap_or("-"));
                println!("model:    {}", s.model.as_deref().unwrap_or("-"));
                println!("status:   {}", s.status);
                println!("started:  {}", s.started_at);
                if let Some(e) = &s.ended_at {
                    println!("ended:    {e}");
                }
                println!(
                    "tokens:   {} prompt + {} completion",
                    s.prompt_tokens, s.completion_tokens
                );
                let cost_str = xfade_core::proxy::pricing::format_cost(s.cost_usd_micros);
                println!("cost:     {cost_str} (estimated)");
                if let Some(p) = &s.first_prompt {
                    println!("opener:   {p}");
                }
                let hs = core.db().list_handovers(&s.id)?;
                if !hs.is_empty() {
                    println!("model switches:");
                    for h in &hs {
                        let inj = if h.injected { " (injected)" } else { "" };
                        println!(
                            "  {}  {} -> {}{}",
                            h.ts,
                            h.from_model.as_deref().unwrap_or("-"),
                            h.to_model.as_deref().unwrap_or("-"),
                            inj
                        );
                    }
                }
                Ok(())
            }
        },
        Cmd::Pricing { cmd } => cmd_pricing(&build_core()?, cmd),
        Cmd::Project => {
            let core = build_core()?;
            let cwd = std::env::current_dir()?;
            let map = xfade_core::analyzer::analyze(&cwd, core.db());
            println!("root: {}", map.root);
            println!("lang: {}", map.lang.unwrap_or("(unknown)"));
            match &map.git {
                Some(g) => {
                    println!("branch: {}", g.branch);
                    println!("head: {}", g.head);
                    for s in &g.recent_subjects {
                        println!("  recent: {s}");
                    }
                }
                None => println!("git: (not a repo)"),
            }
            Ok(())
        }
        Cmd::Handover { copy, raw } => cmd_handover(copy, raw),
        Cmd::SelfUpdate => self_update(),
        Cmd::Tui { tool } => {
            let core = build_core()?;
            let tool = match tool {
                Some(t) => t,
                None => resolve_tui_tool(&core)?,
            };
            tui::run_tui(&core, tool)
        }
    }
}

/// Choose a default tool for the TUI: the first tool that has providers, else claude.
fn resolve_tui_tool(core: &Core) -> Result<ToolKind, CoreError> {
    for t in ToolKind::ALL {
        if !core.list(Some(t))?.is_empty() {
            return Ok(t);
        }
    }
    Ok(ToolKind::ClaudeCode)
}

/// Build the host target triple matching the release asset names.
fn host_target() -> String {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Find `asset`'s digest in a `sha256sum`-style listing (`<hex>  <name>`,
/// optionally `<hex> *<name>`).
fn expected_sha256(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?.trim_start_matches('*');
        (name == asset && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Update xfade to the latest GitHub release: query the latest tag, download the
/// matching platform asset, extract it with system `tar`, and swap the running binary.
fn self_update() -> Result<(), CoreError> {
    let target = host_target();
    let asset = format!("xfade-{target}.tar.gz");
    let current = env!("CARGO_PKG_VERSION");

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| CoreError::Proxy(format!("create tokio runtime: {e}")))?;
    rt.block_on(async {
        let client = reqwest::Client::new();
        let resp: serde_json::Value = client
            .get("https://api.github.com/repos/xfade-dev/xfade/releases/latest")
            .header("User-Agent", "xfade-self-update")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| CoreError::Proxy(format!("query latest release: {e}")))?
            .json()
            .await
            .map_err(|e| CoreError::Proxy(format!("parse release: {e}")))?;

        if let Some(msg) = resp.get("message").and_then(|m| m.as_str()) {
            return Err(CoreError::Proxy(format!("GitHub API: {msg}")));
        }
        let tag = resp
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_start_matches('v');
        if tag == current {
            println!("xfade {current} is already the latest version");
            return Ok(());
        }

        let url = resp
            .get("assets")
            .and_then(|v| v.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(asset.as_str()))
            })
            .and_then(|a| a.get("browser_download_url").and_then(|u| u.as_str()))
            .ok_or_else(|| CoreError::Proxy(format!("no asset '{asset}' in release {tag}")))?
            .to_string();

        // Fail closed: the release must publish SHA256SUMS listing this asset.
        let sums_url = resp
            .get("assets")
            .and_then(|v| v.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|a| a.get("name").and_then(|n| n.as_str()) == Some("SHA256SUMS"))
            })
            .and_then(|a| a.get("browser_download_url").and_then(|u| u.as_str()))
            .ok_or_else(|| {
                CoreError::Proxy(format!(
                    "release {tag} has no SHA256SUMS; refusing to install an unverified binary"
                ))
            })?
            .to_string();
        let sums = client
            .get(&sums_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| CoreError::Proxy(format!("download SHA256SUMS: {e}")))?
            .text()
            .await
            .map_err(|e| CoreError::Proxy(format!("download SHA256SUMS: {e}")))?;
        let expected = expected_sha256(&sums, &asset).ok_or_else(|| {
            CoreError::Proxy(format!(
                "'{asset}' is not listed in SHA256SUMS; refusing to install"
            ))
        })?;

        println!("xfade: updating {current} -> {tag} ({asset})");
        let bytes = client
            .get(&url)
            .send()
            .await
            .map_err(|e| CoreError::Proxy(format!("download: {e}")))?
            .bytes()
            .await
            .map_err(|e| CoreError::Proxy(format!("download: {e}")))?;

        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(CoreError::Proxy(format!(
                "checksum mismatch for {asset}: expected {expected}, got {actual}; aborting update"
            )));
        }
        println!("xfade: checksum verified");

        let tmp = std::env::temp_dir().join(format!("xfade-update-{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        let tarball = tmp.join("xfade.tar.gz");
        std::fs::write(&tarball, &bytes)?;
        let out = std::process::Command::new("tar")
            .args([
                "-xzf",
                tarball.to_str().unwrap_or(""),
                "-C",
                tmp.to_str().unwrap_or(""),
            ])
            .status()
            .map_err(|e| CoreError::Proxy(format!("run tar: {e}")))?;
        if !out.success() {
            return Err(CoreError::Proxy("tar extraction failed".into()));
        }

        let new_bin = tmp.join("xfade");
        let current_exe =
            std::env::current_exe().map_err(|e| CoreError::Proxy(format!("current_exe: {e}")))?;
        // Atomic replace: stage in the same directory, then rename. Renaming
        // also replaces a symlink itself instead of writing through to its
        // target (e.g. a dev-directory binary linked into /usr/local/bin,
        // which a later `cargo build` would fight over).
        if std::fs::symlink_metadata(&current_exe)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            println!(
                "xfade: {} is a symlink; replacing the link itself (its target is left untouched)",
                current_exe.display()
            );
        }
        let dir = current_exe
            .parent()
            .ok_or_else(|| CoreError::Proxy("current_exe has no parent dir".into()))?;
        let staged = dir.join(format!(".xfade-update-{}", std::process::id()));
        let replace_err = |e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                CoreError::Proxy(format!(
                    "replace binary ({}): permission denied — \
                     run `sudo xfade self-update` to overwrite a system-installed binary",
                    current_exe.display()
                ))
            } else {
                CoreError::Proxy(format!("replace binary ({}): {e}", current_exe.display()))
            }
        };
        std::fs::copy(&new_bin, &staged).map_err(replace_err)?;
        std::fs::rename(&staged, &current_exe).map_err(replace_err)?;
        println!("xfade: updated to {tag}");
        Ok(())
    })
}

/// Parse `<N>d` into an RFC3339 timestamp prefix (now - N days).
fn parse_since(s: &str) -> Result<String, CoreError> {
    let s = s.trim();
    let days = if let Some(rest) = s.strip_suffix('d') {
        rest.parse::<u64>().map_err(|_| CoreError::ConfigParse {
            path: String::new(),
            msg: format!("invalid --since {s:?}; expected <N>d, e.g. 7d"),
        })?
    } else {
        return Err(CoreError::ConfigParse {
            path: String::new(),
            msg: format!("--since only supports <N>d (e.g. 7d); got {s:?}"),
        });
    };
    let now = time::OffsetDateTime::now_utc() - time::Duration::days(days as i64);
    now.format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| CoreError::ConfigParse {
            path: "time".into(),
            msg: e.to_string(),
        })
}

fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;

    #[cfg(target_os = "windows")]
    let mut child = Command::new("clip").stdin(Stdio::piped()).spawn()?;

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut child = {
        if let Ok(c) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
            c
        } else if let Ok(c) = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
        {
            c
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither 'wl-copy' nor 'xclip' found",
            ));
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "clipboard command exited with status {status}"
        )))
    }
}

fn format_circuit_badge(state: CircuitState, cooldown: Option<u64>) -> String {
    use crossterm::style::Stylize;
    match state {
        CircuitState::Closed => format!("{}", "[closed]".green()),
        CircuitState::HalfOpen => format!("{}", "[HALF-OPEN]".yellow()),
        CircuitState::Open => {
            if let Some(secs) = cooldown {
                format!("{}", format!("[OPEN (cooldown: {secs}s)]").red())
            } else {
                format!("{}", "[OPEN]".red())
            }
        }
    }
}

fn cmd_audit(
    core: &Core,
    tool: Option<ToolKind>,
    limit: usize,
    export: bool,
    format: &str,
) -> Result<(), CoreError> {
    if export {
        let logs = core.export_audit_logs(None)?;
        if format.eq_ignore_ascii_case("csv") {
            println!("id,ts,client_tool,client_token_id,provider_id,upstream_domain,upstream_model,key_ref,status_code,duration_ms,prompt_tokens,completion_tokens,failover_path");
            for l in &logs {
                println!(
                    "{},{},{},{},{},{},{},{},{},{},{},{},\"{}\"",
                    l.id,
                    l.ts,
                    l.client_tool,
                    l.client_token_id,
                    l.provider_id,
                    l.upstream_domain,
                    l.upstream_model,
                    l.key_ref,
                    l.status_code,
                    l.duration_ms,
                    l.prompt_tokens,
                    l.completion_tokens,
                    l.failover_path.as_deref().unwrap_or("")
                );
            }
        } else {
            let s = serde_json::to_string_pretty(&logs).map_err(|e| CoreError::ConfigParse {
                path: "audit_export".into(),
                msg: e.to_string(),
            })?;
            println!("{s}");
        }
        return Ok(());
    }

    let logs = core.recent_audit_logs(limit, tool)?;
    println!("=== xfade Security Audit Log ===");
    if logs.is_empty() {
        println!("No audit records found.");
        return Ok(());
    }

    println!("Showing {} recent audit record(s):", logs.len());
    println!(
        "{:<19}  {:<10}  {:<12}  {:<20}  {:<18}  {:<6}  {:<6}  KEY REF",
        "TIME (UTC)", "CLIENT", "PROVIDER", "DOMAIN", "MODEL", "STATUS", "LATENCY"
    );
    println!("{}", "-".repeat(110));

    for l in &logs {
        use crossterm::style::Stylize;
        let dt = time::OffsetDateTime::from_unix_timestamp((l.ts / 1000) as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        let time_str = dt
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "-".into());
        let time_short = &time_str[..19.min(time_str.len())];

        let status_str = format!("{:>3}", l.status_code);
        let status_colored = if l.status_code < 400 {
            format!("{}", status_str.as_str().green())
        } else {
            format!("{}", status_str.as_str().red())
        };

        println!(
            "{:<19}  {:<10}  {:<12}  {:<20}  {:<18}  {}   {:>4}ms  {}",
            time_short,
            l.client_tool,
            l.provider_id,
            l.upstream_domain,
            if l.upstream_model.is_empty() {
                "-"
            } else {
                &l.upstream_model
            },
            status_colored,
            l.duration_ms,
            l.key_ref,
        );
    }
    Ok(())
}

fn cmd_doctor(core: &Core, fix: bool) -> Result<(), CoreError> {
    if fix {
        let actions = core.security_doctor_fix()?;
        if actions.is_empty() {
            println!("No security issues found to fix.");
        } else {
            println!("Remediated {} security issue(s):", actions.len());
            for a in &actions {
                println!("  ✓ {a}");
            }
            println!();
        }
    }

    let report = core.security_doctor()?;
    println!("=== xfade Security Doctor ===");
    println!(
        "Trust Status: {}",
        match report.overall_trust {
            xfade_core::security::TrustState::Guarded => "GUARDED (Zero-Raw-Key Gateway Isolated)",
            xfade_core::security::TrustState::Direct => "DIRECT (Direct Upstream Connection)",
            xfade_core::security::TrustState::Exposed => {
                "EXPOSED (Insecure Key or Permissions Detected)"
            }
        }
    );
    println!();

    println!("Tools:");
    for t in &report.tool_trust {
        let icon = match t.trust {
            xfade_core::security::TrustState::Guarded => "[GUARDED]",
            xfade_core::security::TrustState::Direct => "[DIRECT ]",
            xfade_core::security::TrustState::Exposed => "[EXPOSED]",
        };
        println!("  {:<10} {:<10} - {}", t.tool, icon, t.summary);
    }
    println!();

    println!("Security Checks:");
    for c in &report.checks {
        let badge = match c.status {
            xfade_core::security::CheckStatus::Pass => "PASS",
            xfade_core::security::CheckStatus::Warn => "WARN",
            xfade_core::security::CheckStatus::Fail => "FAIL",
        };
        println!("  [{badge}] {:<35} - {}", c.name, c.details);
    }

    if report.overall_trust == xfade_core::security::TrustState::Exposed && !fix {
        println!();
        println!("Hint: Run `xfade doctor --security --fix` to automatically remediate permissions and sanitize configs.");
    }

    Ok(())
}

fn cmd_egress(core: &Core, cmd: Option<EgressCmd>) -> Result<(), CoreError> {
    match cmd {
        None | Some(EgressCmd::List) => {
            let policy = core.get_egress_policy()?;
            let custom_domains = core.list_egress_domains()?;
            let providers = core.list(None)?;
            let mut registered_domains: Vec<String> = providers
                .into_iter()
                .filter_map(|p| p.base_url)
                .map(|u| xfade_core::store::db::normalize_domain(&u))
                .filter(|d| !d.is_empty())
                .collect();
            registered_domains.sort();
            registered_domains.dedup();

            println!("=== xfade Outbound Egress Policy ===");
            println!("Policy Mode: {policy}");
            println!();

            println!("Registered Provider Domains (automatic):");
            if registered_domains.is_empty() {
                println!("  (none)");
            } else {
                for d in &registered_domains {
                    println!("  - {d}");
                }
            }
            println!();

            println!("Custom Allowed Domains (whitelist):");
            if custom_domains.is_empty() {
                println!("  (none)");
            } else {
                for d in &custom_domains {
                    println!("  - {d}");
                }
            }
        }
        Some(EgressCmd::Add { domain }) => {
            let norm = xfade_core::store::db::normalize_domain(&domain);
            if norm.is_empty() {
                println!("Invalid domain: '{domain}'");
                return Ok(());
            }
            core.add_egress_domain(&norm)?;
            println!("Added '{norm}' to egress whitelist.");
        }
        Some(EgressCmd::Remove { domain }) => {
            let norm = xfade_core::store::db::normalize_domain(&domain);
            if core.remove_egress_domain(&norm)? {
                println!("Removed '{norm}' from egress whitelist.");
            } else {
                println!("Domain '{norm}' was not found in egress whitelist.");
            }
        }
        Some(EgressCmd::Policy { mode }) => {
            let policy: xfade_core::store::db::EgressPolicy =
                mode.parse().map_err(|e| CoreError::ConfigParse {
                    path: "egress_policy".into(),
                    msg: e,
                })?;
            core.set_egress_policy(policy)?;
            println!("Egress policy set to '{policy}'.");
        }
    }
    Ok(())
}

fn cmd_model_map(core: &Core, cmd: ModelMapCmd) -> Result<(), CoreError> {
    match cmd {
        ModelMapCmd::Set {
            alias,
            mappings,
            force,
        } => {
            let alias = alias.trim();
            if alias.is_empty() {
                return Err(CoreError::ConfigParse {
                    path: "model-map".into(),
                    msg: "model alias cannot be empty".into(),
                });
            }
            if mappings.is_empty() {
                return Err(CoreError::ConfigParse {
                    path: "model-map".into(),
                    msg: "expected at least one <provider>=<model> mapping".into(),
                });
            }
            let mut clean_mappings = Vec::with_capacity(mappings.len());
            let mut seen = std::collections::HashSet::new();
            for (p, m) in mappings {
                let p = p.trim().to_string();
                let m = m.trim().to_string();
                if p.is_empty() || m.is_empty() {
                    return Err(CoreError::ConfigParse {
                        path: "model-map".into(),
                        msg: format!(
                            "expected <provider>=<model> with non-empty parts, got {p:?}={m:?}"
                        ),
                    });
                }
                if !seen.insert(p.clone()) {
                    return Err(CoreError::ConfigParse {
                        path: "model-map".into(),
                        msg: format!("duplicate provider '{p}' in mapping arguments"),
                    });
                }
                clean_mappings.push((p, m));
            }
            if !force {
                let known: std::collections::HashSet<String> =
                    core.list(None)?.into_iter().map(|p| p.id).collect();
                for (p, _) in &clean_mappings {
                    if p != "*" && !known.contains(p) {
                        return Err(CoreError::ProviderNotFound(format!(
                            "'{p}' (not in any tool; run `xfade ls` to list, or use `--force`)"
                        )));
                    }
                }
            }
            core.db().set_model_mappings_batch(alias, &clean_mappings)?;
            println!("Updated model mapping for alias '{alias}':");
            for (p, m) in &clean_mappings {
                println!("  {p} => {m}");
            }
            Ok(())
        }
        ModelMapCmd::Ls => {
            let mappings = core.db().get_model_mappings()?;
            if mappings.is_empty() {
                println!("(no model mappings configured; use `xfade model-map set <alias> <provider>=<model>...`)");
                return Ok(());
            }
            let mut alias_w = "ALIAS".len();
            let mut prov_w = "PROVIDER".len();
            for (alias, prov_map) in &mappings {
                alias_w = alias_w.max(alias.len());
                for prov in prov_map.keys() {
                    prov_w = prov_w.max(prov.len());
                }
            }
            alias_w += 2;
            prov_w += 2;
            println!(
                "{:<alias_w$} {:<prov_w$} UPSTREAM MODEL",
                "ALIAS", "PROVIDER"
            );
            println!("{}", "─".repeat(alias_w + prov_w + 20));
            for (alias, prov_map) in mappings {
                for (prov, target) in prov_map {
                    println!("{:<alias_w$} {:<prov_w$} {}", alias, prov, target);
                }
            }
            Ok(())
        }
        ModelMapCmd::Rm { alias, provider } => {
            let alias = alias.trim();
            let removed = core.db().remove_model_mapping(alias, provider.as_deref())?;
            if removed {
                match provider {
                    Some(p) => println!("Removed mapping for provider '{p}' from alias '{alias}'."),
                    None => println!("Removed all mappings for alias '{alias}'."),
                }
            } else {
                match provider {
                    Some(p) => {
                        println!("No mapping found for provider '{p}' under alias '{alias}'.")
                    }
                    None => println!("No mappings found for alias '{alias}'."),
                }
            }
            Ok(())
        }
        ModelMapCmd::Clear => {
            core.db().clear_model_mappings()?;
            println!("Cleared all model mappings.");
            Ok(())
        }
    }
}

fn cmd_pricing(core: &Core, cmd: Option<PricingCmd>) -> Result<(), CoreError> {
    let now_rfc3339 = || {
        use time::format_description::well_known::Rfc3339;
        time::OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
    };

    match cmd.unwrap_or(PricingCmd::List { filter: None }) {
        PricingCmd::List { filter } => {
            let filter_lower = filter.as_deref().map(|f| f.trim().to_lowercase());
            // 1. Collect built-ins
            let mut models_map: std::collections::BTreeMap<
                String,
                (i64, i64, i64, i64, &'static str, String),
            > = std::collections::BTreeMap::new();
            for (prefix, price) in xfade_core::proxy::pricing::list_builtin_catalog() {
                models_map.insert(
                    prefix.to_string(),
                    (
                        price.input_micros_per_m,
                        price.output_micros_per_m,
                        price.cache_read_micros_per_m,
                        price.cache_write_micros_per_m,
                        "builtin",
                        "-".to_string(),
                    ),
                );
            }

            // 2. Overlay synced and custom from DB (custom rows must never be overwritten by synced rows)
            let db_rows = core.db().list_model_pricing()?;
            for row in db_rows {
                let is_custom = row.source == "custom";
                let source_str = if is_custom { "custom" } else { "synced" };
                if let Some((_, _, _, _, existing_source, _)) = models_map.get(&row.model_pattern) {
                    if *existing_source == "custom" && !is_custom {
                        continue;
                    }
                }
                models_map.insert(
                    row.model_pattern,
                    (
                        row.input_micros_per_m,
                        row.output_micros_per_m,
                        row.cache_read_micros_per_m,
                        row.cache_write_micros_per_m,
                        source_str,
                        row.updated_at,
                    ),
                );
            }

            let mut count = 0;
            println!(
                "{:<32} {:>12} {:>12} {:>14} {:>14} {:<8} {:<24}",
                "MODEL",
                "INPUT ($/1M)",
                "OUTPUT ($/1M)",
                "CACHE READ",
                "CACHE WRITE",
                "SOURCE",
                "UPDATED AT"
            );
            println!("{}", "-".repeat(105));

            for (pattern, (inp, out, cr, cw, src, upd)) in models_map {
                if let Some(ref f) = filter_lower {
                    if !pattern.to_lowercase().contains(f) {
                        continue;
                    }
                }
                count += 1;
                let inp_str = format!("${:.4}", inp as f64 / 1_000_000.0);
                let out_str = format!("${:.4}", out as f64 / 1_000_000.0);
                let cr_str = format!("${:.4}", cr as f64 / 1_000_000.0);
                let cw_str = format!("${:.4}", cw as f64 / 1_000_000.0);
                println!(
                    "{:<32} {:>12} {:>12} {:>14} {:>14} {:<8} {}",
                    pattern, inp_str, out_str, cr_str, cw_str, src, upd
                );
            }
            println!("\nTotal: {} models listed.", count);
            Ok(())
        }
        PricingCmd::Sync { url } => {
            let target_url = url
                .as_deref()
                .unwrap_or(xfade_core::proxy::DEFAULT_OPENROUTER_MODELS_URL);
            println!("Syncing pricing from {target_url}...");
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| CoreError::Proxy(format!("failed to start tokio runtime: {e}")))?;
            let stats = rt.block_on(xfade_core::proxy::sync_pricing_catalog(
                core.db(),
                url.as_deref(),
            ))?;
            println!(
                "synced: {} models loaded into local catalog.",
                stats.models_synced
            );
            Ok(())
        }
        PricingCmd::Set {
            model,
            input,
            output,
            cache_read,
            cache_write,
        } => {
            let model_trimmed = model.trim().to_lowercase();
            if model_trimmed.is_empty() {
                return Err(CoreError::Proxy(
                    "model pattern cannot be empty".to_string(),
                ));
            }
            if !input.is_finite() || input < 0.0 || !output.is_finite() || output < 0.0 {
                return Err(CoreError::Proxy(
                    "pricing rates must be non-negative finite numbers".to_string(),
                ));
            }
            if let Some(cr) = cache_read {
                if !cr.is_finite() || cr < 0.0 {
                    return Err(CoreError::Proxy(
                        "cache_read rate must be a non-negative finite number".to_string(),
                    ));
                }
            }
            if let Some(cw) = cache_write {
                if !cw.is_finite() || cw < 0.0 {
                    return Err(CoreError::Proxy(
                        "cache_write rate must be a non-negative finite number".to_string(),
                    ));
                }
            }
            let input_micros = (input * 1_000_000.0).round() as i64;
            let output_micros = (output * 1_000_000.0).round() as i64;
            let cache_read_micros = cache_read
                .map(|r| (r * 1_000_000.0).round() as i64)
                .unwrap_or((input_micros as f64 * 0.1).round() as i64);
            let cache_write_micros = cache_write
                .map(|w| (w * 1_000_000.0).round() as i64)
                .unwrap_or((input_micros as f64 * 1.25).round() as i64);

            let row = xfade_core::store::db::DynamicPriceRow {
                model_pattern: model_trimmed.clone(),
                input_micros_per_m: input_micros,
                output_micros_per_m: output_micros,
                cache_read_micros_per_m: cache_read_micros,
                cache_write_micros_per_m: cache_write_micros,
                source: "custom".to_string(),
                updated_at: now_rfc3339(),
            };
            core.db().upsert_model_pricing(&row)?;

            // If pattern is namespaced (e.g. "anthropic/claude-sonnet-5.5"), also alias bare name
            if let Some((_, bare)) = model_trimmed.rsplit_once('/') {
                let bare = bare.trim();
                if !bare.is_empty() && bare != model_trimmed {
                    let mut bare_row = row.clone();
                    bare_row.model_pattern = bare.to_string();
                    let _ = core.db().upsert_model_pricing(&bare_row);
                }
            }

            println!(
                "Set custom pricing for {}: ${:.4} in / ${:.4} out per 1M tokens.",
                model, input, output
            );
            Ok(())
        }
        PricingCmd::Rm { model } => {
            let model_trimmed = model.trim().to_lowercase();
            if model_trimmed.is_empty() {
                return Err(CoreError::Proxy(
                    "model pattern cannot be empty".to_string(),
                ));
            }
            let mut deleted = core.db().delete_model_pricing(&model_trimmed)?;
            if let Some((_, bare)) = model_trimmed.rsplit_once('/') {
                let bare = bare.trim();
                if !bare.is_empty() && bare != model_trimmed {
                    deleted |= core.db().delete_model_pricing(bare)?;
                }
            }
            if deleted {
                println!("Removed custom pricing override for {model}.");
            } else {
                println!("No custom pricing override found for {model}.");
            }
            Ok(())
        }
        PricingCmd::Reset { synced } => {
            if synced {
                let cleared = core.db().clear_synced_model_pricing()?;
                println!("Cleared {cleared} synced model pricing entries.");
                Ok(())
            } else {
                Err(CoreError::Proxy(
                    "missing required flag: --synced (e.g. `xfade pricing reset --synced`)"
                        .to_string(),
                ))
            }
        }
    }
}

fn cmd_status(core: &Core) -> Result<(), CoreError> {
    let trust = core
        .trust_state()
        .unwrap_or(xfade_core::security::TrustState::Guarded);
    println!("trust: {trust}");
    let routes_opt = core.db().get_routes()?;
    match &routes_opt {
        None => println!("routes: (none)"),
        Some((routes, model_override, target_protocol)) => {
            let main = &routes[0];
            let backups: Vec<&str> = routes.iter().skip(1).map(|s| s.as_str()).collect();
            if backups.is_empty() {
                println!("routes: {main}");
            } else {
                println!("routes: {main} -> {}", backups.join(" -> "));
            }
            match model_override.as_deref() {
                Some(m) => println!("model: {m}"),
                None => println!("model: (passthrough)"),
            }
            println!("target: {target_protocol}");
        }
    }
    let smart = if core.db().smart_enabled()? {
        "on"
    } else {
        "off"
    };
    println!("smart fader: {smart}");
    let route_models = core.db().get_route_models()?;
    if route_models.is_empty() {
        println!("route models: (none)");
    } else {
        println!("route models:");
        for (id, m) in &route_models {
            println!("  {id} = {m}");
        }
    }
    let model_mappings = core.db().get_model_mappings()?;
    if model_mappings.is_empty() {
        println!("model mappings: (none)");
    } else {
        println!("model mappings:");
        for (alias, prov_map) in &model_mappings {
            let entries: Vec<String> = prov_map.iter().map(|(p, m)| format!("{p}={m}")).collect();
            println!("  {alias} => {}", entries.join(", "));
        }
    }
    let failover_4xx = if core.db().get_failover_4xx()? {
        "on"
    } else {
        "off"
    };
    println!("failover 4xx: {failover_4xx}");
    let models_merge = if core.db().models_merge_enabled()? {
        "on"
    } else {
        "off"
    };
    println!("models merge: {models_merge}");
    let handover_inject = if core.db().handover_auto_inject_enabled()? {
        "on"
    } else {
        "off"
    };
    println!("handover inject: {handover_inject}");
    if let Some((root, map_json)) = core.db().latest_repo_map()? {
        let v: serde_json::Value = serde_json::from_str(&map_json).unwrap_or_default();
        let lang = v["lang"].as_str().unwrap_or("?");
        let branch = v["git"]["branch"].as_str().unwrap_or("-");
        println!("project: {root} (lang {lang}, branch {branch})");
    }

    let mut daemon_circuits = None;
    if let Ok(home) = std::env::var("HOME") {
        let dd = xfade_core::daemon::data_dir(std::path::Path::new(&home));
        if let Some(cfg) = xfade_core::daemon::read(&dd) {
            let url = format!("http://{}:{}/__xfade/status", cfg.host, cfg.port);
            if let Ok(client) = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(500))
                .build()
            {
                let mut req = client.get(&url);
                if let Some(t) = &cfg.auth_token {
                    req = req.header("Authorization", format!("Bearer {t}"));
                }
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    if let Ok(resp) = rt.block_on(req.send()) {
                        if resp.status().is_success() {
                            if let Ok(dto) =
                                rt.block_on(resp.json::<xfade_core::proxy::status::StatusDto>())
                            {
                                daemon_circuits = Some(dto.circuits);
                            }
                        }
                    }
                }
            }
        }
    }

    println!("circuits:");
    if let Some(circuits) = daemon_circuits {
        if circuits.is_empty() {
            println!("  (none)");
        } else {
            for c in circuits {
                let badge = format_circuit_badge(c.state, c.cooldown_remaining_secs);
                println!("  {}: {badge}", c.provider_id);
            }
        }
    } else if let Some((routes, _, _)) = routes_opt {
        if routes.is_empty() {
            println!("  (none)");
        } else {
            for id in routes {
                let badge = format_circuit_badge(CircuitState::Closed, None);
                println!("  {id}: {badge}");
            }
        }
    } else {
        println!("  (none)");
    }

    Ok(())
}

fn cmd_handover(copy: bool, raw: bool) -> Result<(), CoreError> {
    let core = build_core()?;
    let cwd = std::env::current_dir()?;
    let brief = xfade_core::sessions::handover::collect_handover(
        core.db(),
        &cwd,
        xfade_core::sessions::handover::DEFAULT_MAX_DIFF_CHARS,
    );
    let md = brief.to_markdown();

    if raw {
        println!("{md}");
        return Ok(());
    }

    if copy {
        match copy_to_clipboard(&md) {
            Ok(()) => {
                println!("✓ Handover brief copied to clipboard.");
                println!();
                println!("{md}");
            }
            Err(e) => {
                eprintln!("note: clipboard copy failed ({e}); printing brief below:\n");
                println!("{md}");
            }
        }
    } else {
        println!("{md}");
        println!();
        println!("tip: pass --copy to copy directly to clipboard, or pass --raw for piped output");
    }
    Ok(())
}

/// Interactive-mode decision: skip interactivity once NAME (positional) is provided.
/// NAME present + --base-url absent => official provider (base_url=None), no key needed;
/// NAME absent => interactive: choose a preset or custom.
#[allow(clippy::too_many_arguments)]
fn cmd_add(
    tool: Option<ToolKind>,
    name: Option<String>,
    base_url: Option<String>,
    key: Option<String>,
    protocol: Option<ProviderProtocol>,
    sets: Vec<(String, String)>,
    official: bool,
    slots: PerSlotModels,
) -> Result<(), CoreError> {
    let core = build_core()?;
    let interactive = name.is_none();

    let tool = match tool {
        Some(t) => t,
        None => {
            let items: Vec<String> = ToolKind::ALL.iter().map(|t| t.to_string()).collect();
            let idx = dialoguer::Select::new()
                .with_prompt("Select tool")
                .items(&items)
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?;
            ToolKind::ALL[idx]
        }
    };

    let (name, base_url, mut extra) = match name {
        Some(n) => (n, base_url, serde_json::Value::Null),
        None => {
            let presets = presets_for(tool);
            let labels: Vec<String> = presets
                .iter()
                .map(|p| p.label.to_string())
                .chain(["Custom".into()])
                .collect();
            let idx = dialoguer::Select::new()
                .with_prompt("Select provider")
                .items(&labels)
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?;
            if idx == presets.len() {
                let n = dialoguer::Input::<String>::new()
                    .with_prompt("Name")
                    .interact_text()
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                let u = dialoguer::Input::<String>::new()
                    .with_prompt("Base URL")
                    .interact_text()
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                (n, Some(u), serde_json::Value::Null)
            } else {
                let p = &presets[idx];
                (p.id.to_string(), p.base_url.clone(), p.extra.clone())
            }
        }
    };

    // `--official` (or an interactive pick with no URL, i.e. an official
    // preset) is explicit and must not be replaced by the global base URL.
    // Non-interactive adds with no URL fall back to the global default.
    let force_official = official || (interactive && base_url.is_none());
    let effective_base_url = core.effective_base_url(base_url.as_deref(), force_official);
    let key = if effective_base_url.is_none() {
        None
    } else {
        Some(match key {
            Some(k) => k,
            // Non-interactive: fall back to the global API key when --key is omitted.
            None if !interactive && core.global_api_key().is_some() => {
                core.global_api_key().unwrap_or_default()
            }
            None => dialoguer::Password::new()
                .with_prompt("API Key")
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?,
        })
    };

    let mut map = extra.as_object().cloned().unwrap_or_default();
    for (k, v) in sets {
        map.insert(k, parse_set_value(&v));
    }
    // Explicit per-slot flags win over a `--set` of the same key.
    for (k, v) in slots.entries(tool)? {
        map.insert(k, v);
    }
    if !map.is_empty() {
        extra = serde_json::Value::Object(map);
    }

    let p = core.add_provider_from_input(AddProviderInput {
        id: name.clone(),
        tool,
        base_url,
        api_key: key,
        protocol,
        extra,
        official: force_official,
        // `xfade add` has always upserted; keep that for scripts.
        overwrite: true,
    })?;
    let official = p.is_official();
    println!("added {tool}/{}", p.id);
    if !official {
        println!("run `xfade use {} --tool {tool}` to switch", p.id);
    }
    Ok(())
}

/// `xfade run <name> [--tool t] [-- <args>]`: exec the tool's binary with the
/// provider injected — env for claude/aider, `-c` overrides for codex, an
/// inline `OPENCODE_CONFIG_CONTENT` for opencode; on-disk configs stay
/// untouched. Official providers launch the tool as-is (no injection).
/// Tools without a verified injection contract get a `xfade use` hint.
fn cmd_run(name: &str, tool: Option<ToolKind>, args: &[String]) -> Result<(), CoreError> {
    use std::process::Command;

    let core = build_core()?;
    let tool = resolve_tool(&core, name, tool)?;
    let p = core
        .list(Some(tool))?
        .into_iter()
        .find(|p| p.id == name)
        .ok_or_else(|| CoreError::ProviderNotFound(name.into()))?;

    let key = if p.is_official() {
        None
    } else {
        Some(core.secrets().get(&p.key_ref)?)
    };
    let plan = xfade_core::launch::launch_plan(&p, key.as_deref())?;
    warn_legacy_wire_api(tool);
    warm_project_map(&core);

    let bin = xfade_core::run::resolve_command(xfade_core::run::tool_command(tool));
    let status = Command::new(&bin)
        .args(plan.args_prefix())
        .args(args)
        .envs(plan.env().to_vec())
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("'{bin}' not found in PATH; is {} installed?", tool.label()),
                ))
            } else {
                e.into()
            }
        })?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Cache the launch directory's project fingerprint so handover context
/// (L3) resolves to the project the tool was started in, not the daemon's cwd.
/// Directories without language/git signals are skipped by `analyze`.
fn warm_project_map(core: &Core) {
    if let Ok(cwd) = std::env::current_dir() {
        let _ = xfade_core::analyzer::analyze(&cwd, core.db());
    }
}

/// Newer Codex refuses to start when any ~/.codex/config.toml provider still
/// uses wire_api="chat" (whole-file validation) — warn even though our own
/// injection never writes that value.
fn warn_legacy_wire_api(tool: ToolKind) {
    if tool != ToolKind::Codex {
        return;
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if let Some(w) = xfade_core::launch::codex_legacy_wire_api_warning(&home) {
            eprintln!("[xfade] warning: {w}");
        }
    }
}

/// `xfade <tool> [name] [--model m] ...`: instant-launch flavor of `run` —
/// the tool name is the subcommand, provider/model/key are resolved on the
/// spot (auto-created from a preset or `--base-url` when unregistered) and
/// the tool's on-disk config is never touched. Injection covers claude /
/// aider (env), codex (`-c` overrides + env key) and opencode (inline
/// `OPENCODE_CONFIG_CONTENT`) — codex/opencode verified 2026-10-22 against
/// codex-cli 0.155.0 / opencode 1.18.31; other tools get a `xfade use` hint.
fn cmd_run_instant(tool: ToolKind, opts: RunOpts) -> Result<(), CoreError> {
    use std::process::Command;

    if !matches!(
        tool,
        ToolKind::ClaudeCode | ToolKind::Aider | ToolKind::Codex | ToolKind::OpenCode
    ) {
        return Err(CoreError::ConfigParse {
            path: "run".into(),
            msg: format!(
                "instant launch doesn't support {} (config-file driven; use `xfade use` instead)",
                tool.label()
            ),
        });
    }
    let core = build_core()?;
    let mut p = resolve_instant_provider(&core, tool, &opts)?;

    // One-off overrides on the clone: never persisted, never written to the
    // tool's own config.
    if let Some(m) = &opts.model {
        let mut map = p.extra.as_object().cloned().unwrap_or_default();
        map.insert("model".to_string(), serde_json::Value::String(m.clone()));
        p.extra = serde_json::Value::Object(map);
    }
    if let Some(u) = &opts.base_url {
        p.base_url = Some(u.clone());
    }

    // Key precedence: --key > the provider's stored secret > the global key.
    // A --base-url override that flips an official provider to third-party
    // requires a key.
    let key = match &opts.key {
        Some(k) => Some(k.clone()),
        None if p.is_official() => None,
        None => core
            .secrets()
            .get(&p.key_ref)
            .ok()
            .or_else(|| core.global_api_key()),
    };
    if !p.is_official() && key.is_none() {
        return Err(CoreError::MissingApiKey(format!(
            "provider '{}/{}' has no API key; pass --key or set a global key (`xfade config set api_key <sk-...>`)",
            tool, p.id
        )));
    }

    let plan = xfade_core::launch::launch_plan(&p, key.as_deref())?;
    warn_legacy_wire_api(tool);
    warm_project_map(&core);

    let mut tool_args = opts.args.clone();
    if opts.handover {
        let cwd = std::env::current_dir()?;
        let brief = xfade_core::sessions::handover::collect_handover(
            core.db(),
            &cwd,
            xfade_core::sessions::handover::DEFAULT_MAX_DIFF_CHARS,
        );
        let md = brief.to_markdown();
        let copied = copy_to_clipboard(&md).is_ok();
        eprintln!("╭────────────────────── [xfade] Session Handover ──────────────────────╮");
        if copied {
            eprintln!("│ Context from previous session captured & copied to clipboard!        │");
        } else {
            eprintln!("│ Context from previous session captured successfully.                 │");
        }
        eprintln!("╰──────────────────────────────────────────────────────────────────────╯");

        match tool {
            ToolKind::ClaudeCode | ToolKind::Codex | ToolKind::OpenCode => {
                if tool_args.is_empty() {
                    tool_args.push(format!("Context Handover from previous session:\n\n{md}"));
                } else {
                    let first = &mut tool_args[0];
                    *first =
                        format!("Context Handover from previous session:\n\n{md}\n\nTask: {first}");
                }
            }
            ToolKind::Aider if !tool_args.iter().any(|a| a == "--message" || a == "-m") => {
                tool_args.insert(
                    0,
                    format!("Context Handover from previous session:\n\n{md}"),
                );
                tool_args.insert(0, "--message".into());
            }
            _ => {}
        }
    }

    let bin = xfade_core::run::resolve_command(xfade_core::run::tool_command(tool));
    let status = Command::new(&bin)
        .args(plan.args_prefix())
        .args(&tool_args)
        .envs(plan.env().to_vec())
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("'{bin}' not found in PATH; is {} installed?", tool.label()),
                ))
            } else {
                e.into()
            }
        })?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Resolve the provider for an instant launch. Order:
/// 1. NAME given → registered provider, else preset (auto-created; third-party
///    presets may use the global key, unlike `use`), else `--base-url` ad-hoc
///    creation, else not-found.
/// 2. NAME omitted, `--model` given → reverse lookup over registered
///    providers + presets (id or extra.model, case-insensitive prefix);
///    exactly one hit adopts it, zero/ambiguous error out.
/// 3. Both omitted → the tool's active provider, else a bare official launch.
fn resolve_instant_provider(
    core: &Core,
    tool: ToolKind,
    opts: &RunOpts,
) -> Result<Provider, CoreError> {
    let registered = core.list(Some(tool))?;
    let name = match &opts.name {
        Some(n) => n.clone(),
        None => match &opts.model {
            None => {
                return Ok(core
                    .current(tool)?
                    .unwrap_or_else(|| Provider::new("official", tool, None)));
            }
            Some(m) => match reverse_lookup(&registered, tool, m)? {
                Some(n) => n,
                None => {
                    return Err(CoreError::ProviderNotFound(format!(
                        "no provider or preset matches model '{m}'; pass a name or --base-url"
                    )));
                }
            },
        },
    };

    if let Some(p) = registered.iter().find(|p| p.id == name) {
        return Ok(p.clone());
    }
    if let Some(preset) = presets_for(tool).into_iter().find(|p| p.id == name) {
        let is_local_proxy = name == "local-proxy";
        let preset_official = preset.is_official();
        let mut p = Provider::new(&name, tool, preset.base_url);
        p.extra = preset.extra;
        // Key: official needs none; local-proxy uses the fixed proxy token;
        // third-party: --key or the global key. Unlike `use` (which refuses to
        // auto-create third-party presets — a placeholder written into the
        // tool config breaks its auth), a launch never writes the tool config,
        // so a bad key just fails this one request.
        let key = if preset_official {
            None
        } else if is_local_proxy {
            Some("xfade-local-proxy".to_string())
        } else {
            match &opts.key {
                Some(k) => Some(k.clone()),
                None => core.global_api_key(),
            }
        };
        if !p.is_official() && key.is_none() {
            return Err(CoreError::MissingApiKey(format!(
                "'{name}' is a third-party preset and needs an API key: pass --key, set a global \
                 key (`xfade config set api_key <sk-...>`), or add it first \
                 (`xfade add {name} --tool {tool} --base-url <url> --key <sk-...>`)"
            )));
        }
        core.add_provider(p.clone(), key.as_deref())?;
        println!("auto-created {tool}/{name} from preset");
        return Ok(p);
    }
    if let Some(url) = &opts.base_url {
        let key = match &opts.key {
            Some(k) => Some(k.clone()),
            None => core.global_api_key(),
        };
        if key.is_none() {
            return Err(CoreError::MissingApiKey(format!(
                "creating '{name}' with --base-url needs an API key: pass --key or set a global key"
            )));
        }
        let p = Provider::new(&name, tool, Some(url.clone()));
        core.add_provider(p.clone(), key.as_deref())?;
        println!("auto-created {tool}/{name}");
        return Ok(p);
    }
    Err(CoreError::ProviderNotFound(format!(
        "{name} (run `xfade ls` / `xfade presets` to list, or create ad hoc with --base-url)"
    )))
}

/// Reverse lookup for `xfade <tool> --model <m>` without a name: candidates are
/// registered providers and presets whose `id` or `extra.model` equals or
/// (case-insensitively) prefixes the model. Exactly one candidate adopts it.
fn reverse_lookup(
    registered: &[Provider],
    tool: ToolKind,
    model: &str,
) -> Result<Option<String>, CoreError> {
    let m = model.to_ascii_lowercase();
    let mut hits: Vec<String> = Vec::new();
    for p in registered {
        if model_hit(&p.id, p.extra.get("model").and_then(|v| v.as_str()), &m)
            && !hits.contains(&p.id)
        {
            hits.push(p.id.clone());
        }
    }
    for preset in presets_for(tool) {
        if !hits.contains(&preset.id)
            && model_hit(
                &preset.id,
                preset.extra.get("model").and_then(|v| v.as_str()),
                &m,
            )
        {
            hits.push(preset.id.clone());
        }
    }
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits.remove(0))),
        _ => Err(CoreError::ConfigParse {
            path: String::new(),
            msg: format!("model '{model}' is ambiguous between {hits:?}; pass an explicit name"),
        }),
    }
}

fn model_hit(id: &str, extra_model: Option<&str>, lower_model: &str) -> bool {
    let id = id.to_ascii_lowercase();
    if id == lower_model || lower_model.starts_with(&id) {
        return true;
    }
    match extra_model {
        Some(em) => {
            let em = em.to_ascii_lowercase();
            em == lower_model || lower_model.starts_with(&em)
        }
        None => false,
    }
}

/// When use/rm/edit omit --tool: search the whole DB for a provider with that name;
/// one match → use it; multiple → error prompting for --tool; zero → ProviderNotFound.
fn resolve_tool(core: &Core, name: &str, tool: Option<ToolKind>) -> Result<ToolKind, CoreError> {
    if let Some(t) = tool {
        return Ok(t);
    }
    let matches: Vec<ToolKind> = core
        .list(None)?
        .into_iter()
        .filter(|p| p.id == name)
        .map(|p| p.tool)
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(CoreError::ProviderNotFound(name.into())),
        _ => Err(CoreError::ConfigParse {
            path: String::new(),
            msg: format!("'{name}' exists in multiple tools {matches:?}; pass --tool"),
        }),
    }
}

fn print_table(list: &[Provider]) {
    for p in list {
        let mark = if p.is_active { "*" } else { " " };
        println!(
            "{} {:<10} {:<16} {:<10} {}",
            mark,
            p.tool.as_str(),
            p.id,
            p.protocol.as_str(),
            p.base_url.as_deref().unwrap_or("(official)")
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn expected_sha256_parses_sha256sum_listings() {
        let h1 = "a".repeat(64);
        let h2 = "B".repeat(64);
        let sums = format!(
            "{h1}  xfade-x86_64-unknown-linux-gnu.tar.gz\n{h2} *xfade-aarch64-apple-darwin.tar.gz\nnot-a-hash  xfade-bad.tar.gz\n"
        );
        assert_eq!(
            expected_sha256(&sums, "xfade-x86_64-unknown-linux-gnu.tar.gz"),
            Some(h1)
        );
        assert_eq!(
            expected_sha256(&sums, "xfade-aarch64-apple-darwin.tar.gz"),
            Some("b".repeat(64))
        );
        assert_eq!(expected_sha256(&sums, "xfade-bad.tar.gz"), None);
        assert_eq!(expected_sha256(&sums, "missing.tar.gz"), None);
        assert_eq!(expected_sha256("", "x"), None);
    }

    use super::*;

    fn names(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn models_merge_subcommand_parses_on_off() {
        // Clap would derive `modelsmerge` without the explicit #[command(name)].
        for state in ["on", "off"] {
            Cli::command()
                .try_get_matches_from(["xfade", "proxy", "models-merge", state])
                .unwrap_or_else(|e| panic!("models-merge {state} should parse: {e}"));
        }
        assert!(Cli::command()
            .try_get_matches_from(["xfade", "proxy", "models-merge", "maybe"])
            .is_err());
    }

    #[test]
    fn handover_inject_subcommand_parses_on_off() {
        for state in ["on", "off"] {
            Cli::command()
                .try_get_matches_from(["xfade", "proxy", "handover-inject", state])
                .unwrap_or_else(|e| panic!("handover-inject {state} should parse: {e}"));
        }
        assert!(Cli::command()
            .try_get_matches_from(["xfade", "proxy", "handover-inject", "maybe"])
            .is_err());
    }

    #[test]
    fn build_route_models_collects_valid_entries() {
        let out = build_route_models(
            &names(&["freebuff", "omni"]),
            &kv(&[
                ("freebuff", "z-ai/glm-5.3-flash"),
                ("omni", "auto/best-fast"),
            ]),
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out["freebuff"], "z-ai/glm-5.3-flash");
        assert_eq!(out["omni"], "auto/best-fast");
    }

    #[test]
    fn build_route_models_empty_raw_is_empty_map() {
        assert!(build_route_models(&names(&["yy"]), &[]).unwrap().is_empty());
    }

    #[test]
    fn build_route_models_rejects_unknown_id() {
        let err = build_route_models(&names(&["yy"]), &kv(&[("ghost", "m")])).unwrap_err();
        assert!(err.to_string().contains("unknown provider id"), "{err}");
    }

    #[test]
    fn build_route_models_rejects_duplicate_id() {
        let err =
            build_route_models(&names(&["yy"]), &kv(&[("yy", "m1"), ("yy", "m2")])).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn build_route_models_rejects_empty_parts() {
        let err = build_route_models(&names(&["yy"]), &kv(&[("yy", "")])).unwrap_err();
        assert!(err.to_string().contains("non-empty"), "{err}");
        let err = build_route_models(&names(&["yy"]), &kv(&[("", "m")])).unwrap_err();
        assert!(err.to_string().contains("non-empty"), "{err}");
    }

    fn prov(id: &str, model: Option<&str>) -> Provider {
        let mut p = Provider::new(id, ToolKind::ClaudeCode, Some("https://gw".into()));
        if let Some(m) = model {
            p.extra = serde_json::json!({ "model": m });
        }
        p
    }

    #[test]
    fn reverse_lookup_matches_id_prefix_and_extra_model() {
        let registered = vec![
            prov("kimi", Some("kimi-k2.5")),
            prov("gw2", Some("openai/gpt-5.1")),
        ];
        // id prefix: "kimi" prefixes "kimi-k2.5"
        assert_eq!(
            reverse_lookup(&registered, ToolKind::ClaudeCode, "kimi-k2.5")
                .unwrap()
                .as_deref(),
            Some("kimi")
        );
        // exact extra.model match
        assert_eq!(
            reverse_lookup(&registered, ToolKind::ClaudeCode, "openai/gpt-5.1")
                .unwrap()
                .as_deref(),
            Some("gw2")
        );
        // nothing matches
        assert_eq!(
            reverse_lookup(&registered, ToolKind::ClaudeCode, "nope").unwrap(),
            None
        );
    }

    #[test]
    fn reverse_lookup_ambiguous_errors() {
        // "kimi-k2.5" prefix-matches both ids → ambiguity, not a silent pick.
        let registered = vec![prov("kimi", None), prov("kimi-k2", None)];
        let err = reverse_lookup(&registered, ToolKind::ClaudeCode, "kimi-k2.5").unwrap_err();
        assert!(err.to_string().contains("ambiguous"), "{err}");
    }

    #[test]
    fn reverse_lookup_registered_wins_over_same_id_preset() {
        // The preset table also has a "kimi" entry; the registered hit must
        // dedupe it instead of reporting ambiguity.
        let registered = vec![prov("kimi", None)];
        assert_eq!(
            reverse_lookup(&registered, ToolKind::ClaudeCode, "kimi-k2.5")
                .unwrap()
                .as_deref(),
            Some("kimi")
        );
    }

    #[test]
    fn model_hit_is_case_insensitive() {
        assert!(model_hit("Kimi", None, "kimi-k2.5"));
        assert!(model_hit("x", Some("Moonshot-K2"), "moonshot-k2"));
        assert!(!model_hit("kimi", None, "gpt-5.1"));
    }

    #[test]
    fn test_cli_model_map_subcommand_parsing() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "xfade",
            "model-map",
            "set",
            "sonnet",
            "anthropic=claude-3-7-sonnet",
            "*=claude-3-7-sonnet",
        ])
        .unwrap();
        match cli.cmd {
            Some(Cmd::ModelMap {
                cmd:
                    ModelMapCmd::Set {
                        alias,
                        mappings,
                        force,
                    },
            }) => {
                assert_eq!(alias, "sonnet");
                assert_eq!(mappings.len(), 2);
                assert!(!force);
            }
            _ => panic!("expected ModelMapCmd::Set"),
        }

        // Test mm alias and ls
        let cli_ls = Cli::try_parse_from(["xfade", "mm", "ls"]).unwrap();
        match cli_ls.cmd {
            Some(Cmd::ModelMap {
                cmd: ModelMapCmd::Ls,
            }) => {}
            _ => panic!("expected ModelMapCmd::Ls"),
        }

        // Test rm with and without provider
        let cli_rm_prov =
            Cli::try_parse_from(["xfade", "model-map", "rm", "sonnet", "anthropic"]).unwrap();
        match cli_rm_prov.cmd {
            Some(Cmd::ModelMap {
                cmd: ModelMapCmd::Rm { alias, provider },
            }) => {
                assert_eq!(alias, "sonnet");
                assert_eq!(provider.as_deref(), Some("anthropic"));
            }
            _ => panic!("expected ModelMapCmd::Rm with provider"),
        }

        let cli_rm_all = Cli::try_parse_from(["xfade", "model-map", "rm", "sonnet"]).unwrap();
        match cli_rm_all.cmd {
            Some(Cmd::ModelMap {
                cmd: ModelMapCmd::Rm { alias, provider },
            }) => {
                assert_eq!(alias, "sonnet");
                assert_eq!(provider, None);
            }
            _ => panic!("expected ModelMapCmd::Rm without provider"),
        }

        // Test clear
        let cli_clear = Cli::try_parse_from(["xfade", "model-map", "clear"]).unwrap();
        match cli_clear.cmd {
            Some(Cmd::ModelMap {
                cmd: ModelMapCmd::Clear,
            }) => {}
            _ => panic!("expected ModelMapCmd::Clear"),
        }
    }

    #[test]
    fn test_cmd_model_map_execution_and_validation() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let core = Core::with_paths(
            &home,
            &data,
            Arc::new(xfade_core::store::secrets::MockStore::default()),
        )
        .unwrap();

        // 1. Without registered provider, set fails unless force or wildcard
        let err = cmd_model_map(
            &core,
            ModelMapCmd::Set {
                alias: "sonnet".into(),
                mappings: vec![("ghost".into(), "model-g".into())],
                force: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not in any tool"));

        // 2. Setting with force succeeds
        cmd_model_map(
            &core,
            ModelMapCmd::Set {
                alias: "sonnet".into(),
                mappings: vec![("ghost".into(), "model-g".into())],
                force: true,
            },
        )
        .unwrap();

        // 2b. Empty provider or empty model fails validation
        let err_empty = cmd_model_map(
            &core,
            ModelMapCmd::Set {
                alias: "sonnet".into(),
                mappings: vec![("ghost".into(), "   ".into())],
                force: true,
            },
        )
        .unwrap_err();
        assert!(err_empty.to_string().contains("non-empty parts"));

        // 2c. Duplicate provider in single set command fails validation
        let err_dup = cmd_model_map(
            &core,
            ModelMapCmd::Set {
                alias: "sonnet".into(),
                mappings: vec![("ghost".into(), "m1".into()), ("ghost".into(), "m2".into())],
                force: true,
            },
        )
        .unwrap_err();
        assert!(err_dup.to_string().contains("duplicate provider"));

        // 3. Setting with wildcard succeeds without force
        cmd_model_map(
            &core,
            ModelMapCmd::Set {
                alias: "sonnet".into(),
                mappings: vec![("*".into(), "fallback-sonnet".into())],
                force: false,
            },
        )
        .unwrap();

        let mappings = core.db().get_mappings_for_client_model("sonnet").unwrap();
        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings.get("ghost").unwrap(), "model-g");
        assert_eq!(mappings.get("*").unwrap(), "fallback-sonnet");

        // 4. Listing works
        cmd_model_map(&core, ModelMapCmd::Ls).unwrap();

        // 5. Removing specific provider
        cmd_model_map(
            &core,
            ModelMapCmd::Rm {
                alias: "sonnet".into(),
                provider: Some("ghost".into()),
            },
        )
        .unwrap();
        assert_eq!(
            core.db()
                .get_mappings_for_client_model("sonnet")
                .unwrap()
                .len(),
            1
        );

        // 6. Clearing all
        cmd_model_map(&core, ModelMapCmd::Clear).unwrap();
        assert!(core.db().get_model_mappings().unwrap().is_empty());
    }
}
