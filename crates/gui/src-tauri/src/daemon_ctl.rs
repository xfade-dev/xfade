use crate::dto::ProxyStatus;
use agent_switch_core::daemon::{self, DaemonConfig};
use agent_switch_core::proxy::status::StatusDto;
use reqwest::Client;
use std::path::Path;

fn stopped() -> ProxyStatus {
    ProxyStatus {
        running: false,
        port: None,
        host: None,
        auth_enabled: false,
        circuits: vec![],
    }
}

fn home() -> Result<String, String> {
    std::env::var("HOME").map_err(|_| "HOME not set".to_string())
}

/// 读 daemon.json 配置；未安装返回 None。
pub fn config() -> Option<DaemonConfig> {
    let home = std::env::var("HOME").ok()?;
    daemon::read(&daemon::data_dir(Path::new(&home)))
}

/// 查询 /__asw/status。连不上或鉴权失败返回 stopped。
pub async fn status(host: &str, port: u16, auth_token: Option<&str>) -> ProxyStatus {
    let url = format!("http://{host}:{port}/__asw/status");
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

/// 按 daemon.json 配置查询状态；未安装返回 stopped。
pub async fn status_from_config() -> ProxyStatus {
    match config() {
        Some(cfg) => status(&cfg.host, cfg.port, cfg.auth_token.as_deref()).await,
        None => stopped(),
    }
}

/// 启动 daemon = `launchctl load` 已有 plist（plist 由 `asw serve install` 写入）。
pub fn start() -> Result<(), String> {
    let h = home()?;
    daemon::load(Path::new(&h)).map_err(|e| e.to_string())
}

/// 停止 daemon = `launchctl unload` plist。
pub fn stop() -> Result<(), String> {
    let h = home()?;
    daemon::unload(Path::new(&h)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_switch_core::proxy::ProxyService;
    use agent_switch_core::store::secrets::MockStore;
    use agent_switch_core::Core;
    use std::sync::Arc;

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
}
