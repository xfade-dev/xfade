use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
