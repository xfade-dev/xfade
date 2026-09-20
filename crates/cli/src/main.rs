use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;
use xfade_core::proxy::ProxyService;
use xfade_core::store::db::StatsGroupBy;
use xfade_core::{presets::presets_for, Config, Core, CoreError, Provider, ToolKind};

mod daemon;
mod tui;

#[derive(Parser)]
#[command(
    name = "xfade",
    version,
    about = "Route AI providers for AI coding tools"
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
    },
    /// Show the currently-active provider per tool
    Current,
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
        #[arg(long)]
        auth_token: Option<String>,
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
        #[arg(long, value_parser = ["provider", "model"])]
        by: Option<String>,
    },
    /// Update xfade to the latest GitHub release
    SelfUpdate,
    /// Interactive TUI for switching providers
    Tui {
        #[arg(long)]
        tool: Option<ToolKind>,
    },
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
        /// Override the upstream model field (passthrough the client's model when omitted)
        #[arg(long)]
        model: Option<String>,
        /// Target protocol: chat (OpenAI-compatible) or messages (Anthropic native passthrough)
        #[arg(long, value_parser = ["chat", "messages"], default_value = "chat")]
        target: String,
    },
    /// View current routes
    Status,
    /// Clear routes
    Clear,
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

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s}"))
}

/// --set value: use JSON when it parses (objects/arrays/numbers/booleans), otherwise treat as a string
fn parse_set_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
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
            sets,
            official,
        } => cmd_add(tool, name, base_url, key, sets, official),
        Cmd::Ls { tool } => {
            let core = build_core()?;
            let list = core.list(tool)?;
            print_table(&list);
            Ok(())
        }
        Cmd::Use { name, tool } => {
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
            core.use_provider(tool, &name)?;
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
                        Some(k) => println!("api_key: {}...", &k[..k.len().min(8)]),
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
            cmd,
        } => match cmd {
            None => {
                let core = build_core()?;
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
                                        "daemon: running :{} auth={}",
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
                core.db().set_routes(&names, model.as_deref(), &target)?;
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
                println!("target: {target}");
                Ok(())
            }
            ProxyCmd::Status => {
                let core = build_core()?;
                match core.db().get_routes()? {
                    None => println!("routes: (none)"),
                    Some((routes, model_override, target_protocol)) => {
                        let main = &routes[0];
                        let backups: Vec<&str> =
                            routes.iter().skip(1).map(|s| s.as_str()).collect();
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
                println!("note: circuit status is only visible inside the serve process (a fresh CLI process sees empty circuits)");
                Ok(())
            }
            ProxyCmd::Clear => {
                let core = build_core()?;
                core.db().clear_routes()?;
                println!("routes cleared");
                Ok(())
            }
        },
        Cmd::Stats { since, by } => {
            let since_ts = parse_since(&since)?;
            let group_by = match by.as_deref() {
                Some("model") => StatsGroupBy::Model,
                _ => StatsGroupBy::Provider,
            };
            let core = build_core()?;
            let rows = core.db().stats_since(&since_ts, group_by, None)?;
            if rows.is_empty() {
                println!("no requests in the last {since}");
                return Ok(());
            }
            println!(
                "{:<24} {:>10} {:>10} {:>8} {:>8}",
                "group", "requests", "tokens", "errors", "avg_ms"
            );
            for r in &rows {
                let tokens = r.prompt_tokens + r.completion_tokens;
                println!(
                    "{:<24} {:>10} {:>10} {:>8} {:>8}",
                    r.group, r.requests, tokens, r.errors, r.avg_duration_ms
                );
            }
            Ok(())
        }
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

        println!("xfade: updating {current} -> {tag} ({asset})");
        let bytes = client
            .get(&url)
            .send()
            .await
            .map_err(|e| CoreError::Proxy(format!("download: {e}")))?
            .bytes()
            .await
            .map_err(|e| CoreError::Proxy(format!("download: {e}")))?;

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
        std::fs::copy(&new_bin, &current_exe).map_err(|e| {
            CoreError::Proxy(format!("replace binary ({}): {e}", current_exe.display()))
        })?;
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

/// Interactive-mode decision: skip interactivity once NAME (positional) is provided.
/// NAME present + --base-url absent => official provider (base_url=None), no key needed;
/// NAME absent => interactive: choose a preset or custom.
fn cmd_add(
    tool: Option<ToolKind>,
    name: Option<String>,
    base_url: Option<String>,
    key: Option<String>,
    sets: Vec<(String, String)>,
    official: bool,
) -> Result<(), CoreError> {
    let core = build_core()?;
    let global = core.config();
    let global_key = core.global_api_key();
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

    let (name, mut base_url, mut extra) = match name {
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

    // Non-interactive global defaults: --official wins, then a missing base_url
    // falls back to the global base_url.
    if !interactive {
        if official {
            base_url = None;
        } else if base_url.is_none() {
            base_url = global.base_url.clone();
        }
    }

    let key = if base_url.is_none() {
        None
    } else {
        Some(match key {
            Some(k) => k,
            // Non-interactive: fall back to the global API key when --key is omitted.
            None if !interactive && global_key.is_some() => global_key.clone().unwrap(),
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
    // Non-interactive: global default model applies when the provider didn't set one.
    if !interactive && !map.contains_key("model") {
        if let Some(m) = &global.model {
            map.insert("model".to_string(), serde_json::Value::String(m.clone()));
        }
    }
    // Non-interactive: global default api type (Pi/OMP) applies when not set.
    if !interactive && !map.contains_key("api") {
        if let Some(a) = &global.api {
            map.insert("api".to_string(), serde_json::Value::String(a.clone()));
        }
    }
    if !map.is_empty() {
        extra = serde_json::Value::Object(map);
    }

    let mut p = Provider::new(&name, tool, base_url);
    p.extra = extra;
    let official = p.is_official();
    core.add_provider(p, key.as_deref())?;
    println!("added {tool}/{name}");
    if !official {
        println!("run `xfade use {name} --tool {tool}` to switch");
    }
    Ok(())
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
            "{} {:<10} {:<16} {}",
            mark,
            p.tool.as_str(),
            p.id,
            p.base_url.as_deref().unwrap_or("(official)")
        );
    }
}
