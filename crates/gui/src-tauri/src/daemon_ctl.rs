use crate::dto::ProxyStatus;
use reqwest::Client;
use std::path::Path;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;
use xfade_core::daemon::{self, DaemonConfig};
use xfade_core::proxy::status::StatusDto;

fn stopped() -> ProxyStatus {
    ProxyStatus {
        running: false,
        managed: false,
        port: None,
        host: None,
        auth_enabled: false,
        circuits: vec![],
    }
}

pub fn unmanaged_status(
    host: &str,
    port: u16,
    auth_enabled: bool,
    circuits: Vec<xfade_core::proxy::status::CircuitDto>,
) -> ProxyStatus {
    ProxyStatus {
        running: true,
        managed: false,
        port: Some(port),
        host: Some(host.to_string()),
        auth_enabled,
        circuits,
    }
}

/// Read the daemon.json config; returns None if not installed.
pub fn config() -> Option<DaemonConfig> {
    let home = std::env::var("HOME").ok()?;
    daemon::read(&daemon::data_dir(Path::new(&home)))
}

/// Query /__xfade/status. Returns stopped on connection/auth failure.
pub async fn status(host: &str, port: u16, auth_token: Option<&str>) -> ProxyStatus {
    status_with_managed(host, port, auth_token, true).await
}

async fn status_with_managed(
    host: &str,
    port: u16,
    auth_token: Option<&str>,
    managed: bool,
) -> ProxyStatus {
    let url = format!("http://{host}:{port}/__xfade/status");
    let client = match Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return stopped(),
    };
    let mut req = client.get(&url);
    if let Some(t) = auth_token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => match r.json::<StatusDto>().await {
            Ok(s) => ProxyStatus {
                running: s.running,
                managed,
                port: Some(s.port),
                host: Some(s.host),
                auth_enabled: s.auth_enabled,
                circuits: s.circuits,
            },
            Err(_) => stopped(),
        },
        _ => stopped(),
    }
}

/// Query status per the daemon.json config; falls back to probing 127.0.0.1:24860
/// (reported as `managed: false`) when `daemon.json` is absent.
pub async fn status_from_config() -> ProxyStatus {
    match config() {
        Some(cfg) => status_with_managed(&cfg.host, cfg.port, cfg.auth_token.as_deref(), true).await,
        None => {
            let fallback = status_with_managed("127.0.0.1", 24860, None, false).await;
            if fallback.running {
                fallback
            } else {
                stopped()
            }
        }
    }
}

/// Args for `xfade serve install` (host/port/auth_token).
pub fn install_args(host: &str, port: u16, auth_token: Option<String>) -> Vec<String> {
    let mut v = vec![
        "serve".into(),
        "install".into(),
        "--host".into(),
        host.into(),
        "--port".into(),
        port.to_string(),
    ];
    if let Some(t) = auth_token {
        v.push("--auth-token".into());
        v.push(t);
    }
    v
}

/// Args for `xfade serve uninstall`.
pub fn uninstall_args() -> Vec<String> {
    vec!["serve".into(), "uninstall".into()]
}

/// Start the daemon: run `xfade serve install` via the sidecar (writes the plist + launchctl load).
/// install is idempotent (unload-then-load inside the CLI), so it can be called repeatedly. The
/// sidecar's current_exe points at the in-app xfade, so the plist reference is correct.
pub async fn start(
    app: &AppHandle,
    host: String,
    port: u16,
    auth_token: Option<String>,
) -> Result<(), String> {
    let args = install_args(&host, port, auth_token);
    let status = app
        .shell()
        .sidecar("xfade")
        .map_err(|e| e.to_string())?
        .args(&args)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "xfade serve install failed (exit {:?})",
            status.code()
        ))
    }
}

/// Stop the daemon without deleting `daemon.json`, preserving host/port/auth_token.
pub fn stop_in_home(home: &Path) -> Result<(), String> {
    daemon::unload_job(home).map_err(|e| e.to_string())
}

/// Stop the daemon without uninstalling `daemon.json`.
pub async fn stop() -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || stop_in_home(Path::new(&home)))
        .await
        .map_err(|e| e.to_string())?
}

/// Restart the managed daemon with the current `daemon.json` config (or defaults).
pub async fn restart(app: &AppHandle) -> Result<(), String> {
    let _ = stop().await;
    let cfg = config();
    let (host, port, auth_token) = cfg
        .map(|c| (c.host, c.port, c.auth_token))
        .unwrap_or_else(|| ("127.0.0.1".into(), 24860, None));
    start(app, host, port, auth_token).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xfade_core::proxy::ProxyService;
    use xfade_core::store::secrets::MockStore;
    use xfade_core::Core;

    #[tokio::test]
    async fn status_queries_running_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::with_paths(
            &dir.path().join("home"),
            &dir.path().join("data"),
            Arc::new(MockStore::default()),
        )
        .unwrap();
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let h = tokio::spawn(async move {
            ProxyService::new(core)
                .with_host_port("127.0.0.1", port)
                .serve_with_shutdown(async move {
                    let _ = rx.await;
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let s = status("127.0.0.1", port, None).await;
        assert!(s.running);
        assert_eq!(s.port, Some(port));
        tx.send(()).unwrap();
        h.await.unwrap().unwrap();
    }

    #[test]
    fn config_reads_daemon_json() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::env::set_var("HOME", &home);
        let dd = daemon::data_dir(&home);
        daemon::write(
            &dd,
            &DaemonConfig {
                host: "127.0.0.1".into(),
                port: 24860,
                auth_token: Some("t".into()),
            },
        )
        .unwrap();
        let cfg = config().expect("config should read");
        assert_eq!(cfg.port, 24860);
        assert_eq!(cfg.auth_token.as_deref(), Some("t"));
        std::env::remove_var("HOME");
    }

    #[test]
    fn install_args_shape() {
        let with_tok = install_args("127.0.0.1", 24860, Some("t".into()));
        assert_eq!(
            with_tok,
            vec![
                "serve",
                "install",
                "--host",
                "127.0.0.1",
                "--port",
                "24860",
                "--auth-token",
                "t"
            ]
        );
        let no_tok = install_args("127.0.0.1", 9000, None);
        assert_eq!(
            no_tok,
            vec!["serve", "install", "--host", "127.0.0.1", "--port", "9000"]
        );
    }

    #[test]
    fn uninstall_args_shape() {
        assert_eq!(uninstall_args(), vec!["serve", "uninstall"]);
    }

    #[test]
    fn proxy_status_unmanaged_fallback_sets_managed_false() {
        let s = unmanaged_status("127.0.0.1", 24860, false, vec![]);
        assert!(s.running);
        assert!(!s.managed);
        assert_eq!(s.port, Some(24860));
        assert_eq!(s.host.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn stop_in_dir_preserves_daemon_json() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let dd = daemon::data_dir(&home);
        daemon::write(
            &dd,
            &DaemonConfig {
                host: "127.0.0.1".into(),
                port: 24860,
                auth_token: Some("tok".into()),
            },
        )
        .unwrap();
        stop_in_home(&home).unwrap();
        assert!(daemon::read(&dd).is_some());
    }
}
