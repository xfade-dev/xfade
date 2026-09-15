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
        port: None,
        host: None,
        auth_enabled: false,
        circuits: vec![],
    }
}

/// Read the daemon.json config; returns None if not installed.
pub fn config() -> Option<DaemonConfig> {
    let home = std::env::var("HOME").ok()?;
    daemon::read(&daemon::data_dir(Path::new(&home)))
}

/// Query /__xfade/status. Returns stopped on connection/auth failure.
pub async fn status(host: &str, port: u16, auth_token: Option<&str>) -> ProxyStatus {
    let url = format!("http://{host}:{port}/__xfade/status");
    let client = Client::new();
    let mut req = client.get(&url);
    if let Some(t) = auth_token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => match r.json::<StatusDto>().await {
            Ok(s) => ProxyStatus {
                running: s.running,
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

/// Query status per the daemon.json config; returns stopped if not installed.
pub async fn status_from_config() -> ProxyStatus {
    match config() {
        Some(cfg) => status(&cfg.host, cfg.port, cfg.auth_token.as_deref()).await,
        None => stopped(),
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

/// Stop the daemon: run `xfade serve uninstall` via the sidecar.
pub async fn stop(app: &AppHandle) -> Result<(), String> {
    let args = uninstall_args();
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
            "xfade serve uninstall failed (exit {:?})",
            status.code()
        ))
    }
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
}
