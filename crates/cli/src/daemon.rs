use agent_switch_core::daemon::{self, DaemonConfig};
use agent_switch_core::{CoreError, Result};
use std::path::{Path, PathBuf};

pub const LABEL: &str = "ai.agent-switch.serve";

fn launch_agents_dir(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
}

pub fn plist_path(home: &Path) -> PathBuf {
    launch_agents_dir(home).join(format!("{LABEL}.plist"))
}

pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".config/agent-switch")
}

/// 写 launchd plist + daemon.json；`load=true` 时 launchctl load（macOS）。
/// `load=false` 仅供测试（不实际加载 launchd）。
pub fn install(
    home: &Path,
    host: &str,
    port: u16,
    auth_token: Option<String>,
    load: bool,
) -> Result<()> {
    std::fs::create_dir_all(launch_agents_dir(home))?;
    let exe = std::env::current_exe()
        .map_err(|e| CoreError::Proxy(format!("current_exe: {e}")))?
        .display()
        .to_string();
    let mut plist = format!(
        "<plist version=\"1.0\"><dict>\
         <key>Label</key><string>{LABEL}</string>\
         <key>ProgramArguments</key><array>\
         <string>{exe}</string><string>serve</string>\
         <string>--host</string><string>{host}</string>\
         <string>--port</string><string>{port}</string>"
    );
    if let Some(t) = &auth_token {
        plist.push_str(&format!(
            "<string>--auth-token</string><string>{t}</string>"
        ));
    }
    let dd = data_dir(home);
    std::fs::create_dir_all(&dd)?;
    let log = dd.join("serve.log").display().to_string();
    plist.push_str(&format!(
        "</array>\
         <key>RunAtLoad</key><true/>\
         <key>KeepAlive</key><true/>\
         <key>StandardOutPath</key><string>{log}</string>\
         <key>StandardErrorPath</key><string>{log}</string>\
         </dict></plist>"
    ));
    std::fs::write(plist_path(home), plist)?;
    let cfg = DaemonConfig {
        host: host.to_string(),
        port,
        auth_token,
    };
    daemon::write(&dd, &cfg)?;
    if load {
        let p = plist_path(home);
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &p.display().to_string()])
            .output();
        std::process::Command::new("launchctl")
            .args(["load", &p.display().to_string()])
            .status()
            .map_err(|e| CoreError::Proxy(format!("launchctl load: {e}")))?;
    }
    Ok(())
}

/// 卸载：launchctl unload（`unload=true`）+ 删 plist + 删 daemon.json。幂等。
pub fn uninstall(home: &Path, unload: bool) -> Result<()> {
    let p = plist_path(home);
    if unload && p.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &p.display().to_string()])
            .status();
    }
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(daemon::daemon_json_path(&data_dir(home)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn install_writes_plist_and_daemon_json() {
        let dir = setup_home();
        let home = dir.path();
        let plist = plist_path(home);
        let cfg = daemon::daemon_json_path(&data_dir(home));
        assert!(!plist.exists());
        install(home, "127.0.0.1", 24860, Some("tok".into()), false).unwrap();
        assert!(plist.exists());
        assert!(cfg.exists());
        let plist_txt = std::fs::read_to_string(&plist).unwrap();
        assert!(plist_txt.contains("ai.agent-switch.serve"));
        assert!(plist_txt.contains("serve"));
        assert!(plist_txt.contains("24860"));
        assert!(plist_txt.contains("tok"));
        assert!(plist_txt.contains("RunAtLoad"));
        assert!(plist_txt.contains("KeepAlive"));
        let cfg_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cfg_v["port"], 24860);
        assert_eq!(cfg_v["auth_token"], "tok");
        assert_eq!(cfg_v["host"], "127.0.0.1");
    }

    #[test]
    fn uninstall_removes_files() {
        let dir = setup_home();
        let home = dir.path();
        install(home, "127.0.0.1", 24860, None, false).unwrap();
        uninstall(home, false).unwrap();
        assert!(!plist_path(home).exists());
        assert!(!daemon::daemon_json_path(&data_dir(home)).exists());
    }

    #[test]
    fn uninstall_idempotent_when_not_installed() {
        let dir = setup_home();
        uninstall(dir.path(), false).unwrap();
    }
}
