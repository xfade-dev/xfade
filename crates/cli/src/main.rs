use agent_switch_core::store::secrets::{FileMockStore, KeyringStore, SecretStore};
use agent_switch_core::{presets::presets_for, Core, CoreError, Provider, ToolKind};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "asw", version, about = "Switch API providers for AI coding tools")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 添加 provider（无 --name 时进入交互模式）
    Add {
        #[arg(long)]
        tool: Option<ToolKind>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        key: Option<String>,
        /// 工具特有配置，可重复：--set model=kimi-k2.5
        #[arg(long = "set", value_parser = parse_kv)]
        sets: Vec<(String, String)>,
    },
    /// 列出 providers
    Ls {
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// 切换 provider
    Use {
        name: String,
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// 显示各工具当前生效的 provider
    Current,
    /// 修改 provider
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
    /// 删除 provider（active 不可删）
    Rm {
        name: String,
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// 列出内置预设
    Presets,
    /// 手动导入工具当前配置为 imported 快照
    Import {
        #[arg(long)]
        tool: Option<ToolKind>,
    },
    /// 备份管理
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// 生成 shell 补全
    Completion { shell: Shell },
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

/// --set 值：能解析为 JSON 就用 JSON（支持对象/数组/数字/布尔），否则按字符串
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

/// 环境契约（与 tests/cli.rs 的 asw() 辅助函数严格一致）：
/// - `HOME`：工具配置根。测试注入 tempdir；真实环境即用户主目录。
/// - `ASW_DATA_DIR`：自身数据目录（db/backups），缺省 `$HOME/.config/agent-switch`。
/// - `ASW_MOCK_SECRETS=1`：用文件 MockStore（ASW_DATA_DIR/mock-secrets.json）替代系统钥匙串（仅测试/冒烟）。
fn build_core() -> Result<Core, CoreError> {
    let use_mock = std::env::var("ASW_MOCK_SECRETS").ok().as_deref() == Some("1");
    if let Ok(home) = std::env::var("HOME") {
        let data = std::env::var_os("ASW_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home).join(".config").join("agent-switch"));
        let secrets: Box<dyn SecretStore> = if use_mock {
            Box::new(FileMockStore::new(data.join("mock-secrets.json")))
        } else {
            Box::new(KeyringStore::new())
        };
        return Core::with_paths(std::path::Path::new(&home), &data, secrets);
    }
    Core::for_current_user() // 无 HOME 的极端环境（如部分 Windows 服务）兜底
}

fn run(cli: Cli) -> Result<(), CoreError> {
    match cli.cmd {
        Cmd::Add { tool, name, base_url, key, sets } => cmd_add(tool, name, base_url, key, sets),
        Cmd::Ls { tool } => {
            let core = build_core()?;
            let list = core.list(tool)?;
            print_table(&list);
            Ok(())
        }
        Cmd::Use { name, tool } => {
            let core = build_core()?;
            let tool = resolve_tool(&core, &name, tool)?;
            core.use_provider(tool, &name)?;
            println!("switched {tool} -> {name}");
            Ok(())
        }
        Cmd::Current => {
            let core = build_core()?;
            for t in ToolKind::ALL {
                match core.current(t)? {
                    Some(p) => println!("{t}: {} ({})", p.id, p.base_url.as_deref().unwrap_or("official")),
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
                    println!("  {:<12} {:<20} {}", p.id, p.label, p.base_url.unwrap_or("(官方)"));
                }
            }
            Ok(())
        }
        Cmd::Edit { name, tool, base_url, key, sets } => {
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
    }
}

/// 交互判定：只要 --name 已提供就不再进交互。
/// --name 有 + --base-url 无 => 官方 provider（base_url=None），无需 key；
/// --name 缺失 => 交互：选预设或自定义。
fn cmd_add(
    tool: Option<ToolKind>,
    name: Option<String>,
    base_url: Option<String>,
    key: Option<String>,
    sets: Vec<(String, String)>,
) -> Result<(), CoreError> {
    let core = build_core()?;

    let tool = match tool {
        Some(t) => t,
        None => {
            let items: Vec<String> = ToolKind::ALL.iter().map(|t| t.to_string()).collect();
            let idx = dialoguer::Select::new()
                .with_prompt("选择工具")
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
                .chain(["自定义".into()])
                .collect();
            let idx = dialoguer::Select::new()
                .with_prompt("选择供应商")
                .items(&labels)
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?;
            if idx == presets.len() {
                let n = dialoguer::Input::<String>::new()
                    .with_prompt("名称")
                    .interact_text()
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                let u = dialoguer::Input::<String>::new()
                    .with_prompt("Base URL")
                    .interact_text()
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                (n, Some(u), serde_json::Value::Null)
            } else {
                let p = &presets[idx];
                (p.id.to_string(), p.base_url.map(String::from), p.extra.clone())
            }
        }
    };

    let key = if base_url.is_none() {
        None
    } else {
        Some(match key {
            Some(k) => k,
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
    if !map.is_empty() {
        extra = serde_json::Value::Object(map);
    }

    let mut p = Provider::new(&name, tool, base_url);
    p.extra = extra;
    let official = p.is_official();
    core.add_provider(p, key.as_deref())?;
    println!("added {tool}/{name}");
    if !official {
        println!("run `asw use {name} --tool {tool}` to switch");
    }
    Ok(())
}

/// use/rm/edit 缺省 --tool 时：全库查同名 provider；
/// 唯一 → 用之；多个 → 报错提示加 --tool；零 → ProviderNotFound
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
