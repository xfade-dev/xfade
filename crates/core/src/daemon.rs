use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LABEL: &str = "ai.agent-switch.serve";

/// daemon 配置（host/port/auth_token），CLI install 时写入 daemon.json，
/// 供 status 子命令与 GUI 读取查询 /__asw/status。
#[derive(Serialize, Deserialize)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
    pub auth_token: Option<String>,
}

pub fn daemon_json_path(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.json")
}

/// 读取 daemon.json；不存在或解析失败返回 None。
pub fn read(data_dir: &Path) -> Option<DaemonConfig> {
    serde_json::from_str(&std::fs::read_to_string(daemon_json_path(data_dir)).ok()?).ok()
}

pub fn write(data_dir: &Path, cfg: &DaemonConfig) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(
        daemon_json_path(data_dir),
        serde_json::to_string(cfg).unwrap(),
    )?;
    Ok(())
}

pub fn launch_agents_dir(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
}

pub fn plist_path(home: &Path) -> PathBuf {
    launch_agents_dir(home).join(format!("{LABEL}.plist"))
}

pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".config/agent-switch")
}

/// `launchctl load` 已有 plist。plist 不存在则报错（提示先 `asw serve install`）。
pub fn load(home: &Path) -> Result<()> {
    let p = plist_path(home);
    if !p.exists() {
        return Err(CoreError::Proxy(
            "daemon not installed; run `asw serve install` first".into(),
        ));
    }
    std::process::Command::new("launchctl")
        .args(["load", &p.display().to_string()])
        .status()
        .map_err(|e| CoreError::Proxy(format!("launchctl load: {e}")))?;
    Ok(())
}

/// `launchctl unload` plist。plist 不存在则幂等成功。
pub fn unload(home: &Path) -> Result<()> {
    let p = plist_path(home);
    if p.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &p.display().to_string()])
            .status();
    }
    Ok(())
}
