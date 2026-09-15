use std::path::Path;
use xfade_core::daemon::{self, DaemonConfig};
use xfade_core::{CoreError, Result};

/// Install the daemon: macOS writes a plist, Linux writes a systemd unit, both write daemon.json.
/// `load=true` loads immediately; `load=false` is for tests only.
pub fn install(
    home: &Path,
    host: &str,
    port: u16,
    auth_token: Option<String>,
    load: bool,
) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| CoreError::Proxy(format!("current_exe: {e}")))?
        .display()
        .to_string();
    let cfg = DaemonConfig {
        host: host.to_string(),
        port,
        auth_token,
    };

    #[cfg(target_os = "macos")]
    {
        daemon::write_plist(&exe, &cfg, home)?;
    }
    #[cfg(target_os = "linux")]
    {
        daemon::write_systemd_unit(&exe, &cfg)?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err(CoreError::Proxy(
            "daemon not supported on this platform; use `xfade serve` (foreground) instead".into(),
        ));
    }

    if load {
        daemon::unload(home)?;
        daemon::load(home)?;
    }
    Ok(())
}

/// Uninstall: unload + remove the plist/systemd unit + remove daemon.json. Idempotent.
pub fn uninstall(home: &Path, do_unload: bool) -> Result<()> {
    if do_unload {
        daemon::unload(home)?;
    }
    let _ = std::fs::remove_file(daemon::plist_path(home));
    let _ = std::fs::remove_file(daemon::systemd_unit_path());
    let _ = std::fs::remove_file(daemon::daemon_json_path(&daemon::data_dir(home)));
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
        let plist = daemon::plist_path(home);
        let cfg = daemon::daemon_json_path(&daemon::data_dir(home));
        assert!(!plist.exists());
        install(home, "127.0.0.1", 24860, Some("tok".into()), false).unwrap();
        assert!(plist.exists());
        assert!(cfg.exists());
        let plist_txt = std::fs::read_to_string(&plist).unwrap();
        assert!(plist_txt.contains("ai.xfade.serve"));
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
        assert!(!daemon::plist_path(home).exists());
        assert!(!daemon::daemon_json_path(&daemon::data_dir(home)).exists());
    }

    #[test]
    fn uninstall_idempotent_when_not_installed() {
        let dir = setup_home();
        uninstall(dir.path(), false).unwrap();
    }
}
