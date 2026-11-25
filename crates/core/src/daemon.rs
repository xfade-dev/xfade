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

/// Owner-only file holding the proxy auth token. The service manager's unit
/// passes `--auth-token-file <this>` so the token never appears in process
/// arguments (visible to every local user via `ps`) or in the plist/unit file.
pub fn token_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("auth_token")
}

/// Persist (or remove) the token file for `cfg`; returns its path when a token is set.
fn sync_token_file(data_dir: &Path, cfg: &DaemonConfig) -> Result<Option<PathBuf>> {
    let path = token_file_path(data_dir);
    match cfg.auth_token.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => {
            crate::fsutil::write_private(&path, t.as_bytes())?;
            Ok(Some(path))
        }
        None => {
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

pub fn write(data_dir: &Path, cfg: &DaemonConfig) -> Result<()> {
    // daemon.json can hold the proxy auth token: owner-only.
    crate::fsutil::write_private(
        &daemon_json_path(data_dir),
        serde_json::to_string(cfg).unwrap().as_bytes(),
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
    if let Some(tf) = sync_token_file(&dd, cfg)? {
        plist.push_str(&format!(
            "<string>--auth-token-file</string><string>{}</string>",
            tf.display()
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
    crate::fsutil::write_private(&pp, plist.as_bytes())?;
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
    if let Some(tf) = sync_token_file(&dd, cfg)? {
        exec.push_str(&format!(" --auth-token-file {}", tf.display()));
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
    crate::fsutil::write_private(&up, unit.as_bytes())?;
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

/// Unload the running job WITHOUT removing the plist/systemd unit file.
/// Used by `install` to tear down a previously-loaded job before reloading:
/// the freshly-written plist must survive so `load` can find it. Idempotent —
/// unloading a job that was never loaded only prints a launchctl warning
/// (exit 0) and is ignored.
pub fn unload_job(home: &Path) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    let _ = home;
    #[cfg(target_os = "macos")]
    {
        let p = plist_path(home);
        if p.exists() {
            // Best-effort teardown: a job that was never loaded (or was already
            // unloaded) makes `launchctl unload` print "Unload failed" to stderr;
            // that's the expected common case here, so silence it.
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &p.display().to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop", LABEL])
            .status();
    }
    Ok(())
}

/// Restart the running daemon in place, leaving the plist/systemd unit file
/// untouched. macOS restarts via `launchctl kickstart -k` (kill + relaunch);
/// Linux restarts the systemd user unit. Used by `xfade serve restart` and the
/// version handshake so a replaced binary is actually picked up by the process.
pub fn restart() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{LABEL}", current_uid()?);
        let status = std::process::Command::new("launchctl")
            .args(["kickstart", "-k", &target])
            .status()
            .map_err(|e| CoreError::Proxy(format!("launchctl kickstart: {e}")))?;
        if !status.success() {
            return Err(CoreError::Proxy(format!(
                "launchctl kickstart {target} exited {status}"
            )));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "restart", LABEL])
            .status()
            .map_err(|e| CoreError::Proxy(format!("systemctl restart: {e}")))?;
        if !status.success() {
            return Err(CoreError::Proxy(format!(
                "systemctl restart {LABEL} exited {status}"
            )));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err(CoreError::Proxy(
            "daemon not supported on this platform".into(),
        ));
    }
    Ok(())
}

/// Current user id as a string, for building the launchd `gui/<uid>/<label>` target.
#[cfg(target_os = "macos")]
fn current_uid() -> Result<String> {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| CoreError::Proxy(format!("id -u: {e}")))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        return Err(CoreError::Proxy("id -u returned an empty uid".into()));
    }
    Ok(uid)
}

/// Unload the daemon and remove its plist/systemd unit file (uninstall).
pub fn unload(home: &Path) -> Result<()> {
    unload_job(home)?;
    #[cfg(target_os = "macos")]
    {
        let _ = std::fs::remove_file(plist_path(home));
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", LABEL])
            .status();
        let _ = std::fs::remove_file(systemd_unit_path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_passes_token_via_owner_only_file_not_argv() {
        let home = tempfile::tempdir().unwrap();
        let cfg = DaemonConfig {
            host: "127.0.0.1".into(),
            port: 24860,
            auth_token: Some("s3cr3t-token".into()),
        };
        let pp = write_plist("/usr/bin/xfade", &cfg, home.path()).unwrap();
        let plist = std::fs::read_to_string(&pp).unwrap();
        assert!(!plist.contains("s3cr3t-token"), "token leaked into plist");
        assert!(plist.contains("--auth-token-file"));
        let dd = data_dir(home.path());
        let tf = token_file_path(&dd);
        assert_eq!(std::fs::read_to_string(&tf).unwrap(), "s3cr3t-token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [&tf, &daemon_json_path(&dd)] {
                let m = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_eq!(m, 0o600, "{} must be owner-only", p.display());
            }
        }
    }

    #[test]
    fn removing_token_removes_stale_token_file() {
        let home = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig {
            host: "127.0.0.1".into(),
            port: 1,
            auth_token: Some("t".into()),
        };
        write_plist("/x", &cfg, home.path()).unwrap();
        cfg.auth_token = None;
        let pp = write_plist("/x", &cfg, home.path()).unwrap();
        assert!(!token_file_path(&data_dir(home.path())).exists());
        assert!(!std::fs::read_to_string(pp)
            .unwrap()
            .contains("--auth-token"));
    }
}
