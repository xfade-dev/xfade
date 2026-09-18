use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LABEL: &str = "ai.xfade.serve";

/// Daemon config (host/port/auth_token), written to daemon.json on CLI install and
/// read by the status subcommand and the GUI to query /__xfade/status.
#[derive(Serialize, Deserialize)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
    pub auth_token: Option<String>,
}

/// Whether the current platform supports system-level daemon management (launchd / systemd).
pub fn platform_supported() -> bool {
    cfg!(target_os = "macos") || cfg!(target_os = "linux")
}

/// Return the platform name for user-facing messages.
pub fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "launchd (macOS)"
    } else if cfg!(target_os = "linux") {
        "systemd (Linux)"
    } else {
        "unsupported"
    }
}

pub fn daemon_json_path(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.json")
}

/// Read daemon.json; returns None if missing or unparsable.
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

/// Linux systemd user unit path.
pub fn systemd_unit_path() -> PathBuf {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    dir.join("systemd/user").join(format!("{LABEL}.service"))
}

pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".config/xfade")
}

/// Write the macOS launchd plist (ProgramArguments points at exe) + daemon.json.
/// Returns the written plist path for use by load/unload.
pub fn write_plist(exe: &str, cfg: &DaemonConfig, home: &Path) -> Result<PathBuf> {
    let dd = data_dir(home);
    std::fs::create_dir_all(launch_agents_dir(home))?;
    std::fs::create_dir_all(&dd)?;
    let log = dd.join("serve.log").display().to_string();
    let mut plist = format!(
        "<plist version=\"1.0\"><dict>\
         <key>Label</key><string>{LABEL}</string>\
         <key>ProgramArguments</key><array>\
         <string>{exe}</string><string>serve</string>\
         <string>--host</string><string>{h}</string>\
         <string>--port</string><string>{p}</string>",
        h = cfg.host,
        p = cfg.port
    );
    if let Some(t) = &cfg.auth_token {
        plist.push_str(&format!(
            "<string>--auth-token</string><string>{t}</string>"
        ));
    }
    plist.push_str(&format!(
        "</array>\
         <key>RunAtLoad</key><true/>\
         <key>KeepAlive</key><true/>\
         <key>StandardOutPath</key><string>{log}</string>\
         <key>StandardErrorPath</key><string>{log}</string>\
         </dict></plist>"
    ));
    let pp = plist_path(home);
    std::fs::write(&pp, plist)?;
    write(&dd, cfg)?;
    Ok(pp)
}

/// Write the Linux systemd user unit + daemon.json.
pub fn write_systemd_unit(exe: &str, cfg: &DaemonConfig) -> Result<PathBuf> {
    let dd = data_dir(
        &dirs::home_dir().ok_or_else(|| CoreError::Keyring("cannot locate home dir".into()))?,
    );
    std::fs::create_dir_all(&dd)?;
    let unit_dir = systemd_unit_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&unit_dir)?;
    let mut exec = format!("{} serve --host {} --port {}", exe, cfg.host, cfg.port);
    if let Some(t) = &cfg.auth_token {
        exec.push_str(&format!(" --auth-token {}", t));
    }
    let unit = format!(
        "[Unit]\n\
         Description=Xfade AI Provider Proxy\n\
         After=network.target\n\n\
         [Service]\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        log = dd.join("serve.log").display()
    );
    let up = systemd_unit_path();
    std::fs::write(&up, unit)?;
    write(&dd, cfg)?;
    Ok(up)
}

/// macOS: `launchctl load` the existing plist. Errors if the plist is missing (hinting to run `xfade serve install` first).
pub fn load(home: &Path) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    let _ = home;
    #[cfg(target_os = "macos")]
    {
        let p = plist_path(home);
        if !p.exists() {
            return Err(CoreError::Proxy(
                "daemon not installed; run `xfade serve install` first".into(),
            ));
        }
        std::process::Command::new("launchctl")
            .args(["load", &p.display().to_string()])
            .status()
            .map_err(|e| CoreError::Proxy(format!("launchctl load: {e}")))?;
    }
    #[cfg(target_os = "linux")]
    {
        let p = systemd_unit_path();
        if !p.exists() {
            return Err(CoreError::Proxy(
                "daemon not installed; run `xfade serve install` first".into(),
            ));
        }
        // systemctl --user enable + start
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", LABEL])
            .status();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err(CoreError::Proxy(
            "daemon not supported on this platform".into(),
        ));
    }
    Ok(())
}

/// Unload the daemon.
pub fn unload(home: &Path) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    let _ = home;
    #[cfg(target_os = "macos")]
    {
        let p = plist_path(home);
        if p.exists() {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &p.display().to_string()])
                .status();
            let _ = std::fs::remove_file(&p);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", LABEL])
            .status();
        let p = systemd_unit_path();
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}
