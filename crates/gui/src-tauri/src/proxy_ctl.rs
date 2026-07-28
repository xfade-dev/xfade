use crate::dto::{circuit_dto, ProxyStatus};
use agent_switch_core::proxy::Circuit;
use agent_switch_core::proxy::ProxyService;
use agent_switch_core::Core;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::async_runtime;
use tokio::sync::oneshot;

pub struct ProxyCtl {
    inner: Mutex<Inner>,
}

struct Inner {
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<async_runtime::JoinHandle<Result<(), agent_switch_core::CoreError>>>,
    port: Option<u16>,
    host: Option<String>,
    auth_enabled: bool,
    circuits: Option<Arc<Mutex<HashMap<String, Circuit>>>>,
}

impl Default for ProxyCtl {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                shutdown: None,
                handle: None,
                port: None,
                host: None,
                auth_enabled: false,
                circuits: None,
            }),
        }
    }
}

impl ProxyCtl {
    pub fn status(&self) -> ProxyStatus {
        let inner = self.inner.lock().unwrap();
        let circuits = inner
            .circuits
            .as_ref()
            .map(|arc| {
                let m = arc.lock().unwrap();
                m.iter()
                    .map(|(id, c)| circuit_dto(id, c))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ProxyStatus {
            running: inner.shutdown.is_some(),
            port: inner.port,
            host: inner.host.clone(),
            auth_enabled: inner.auth_enabled,
            circuits,
        }
    }

    pub fn start(
        &self,
        core: Core,
        host: String,
        port: u16,
        auth_token: Option<String>,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.shutdown.is_some() {
            return Err("proxy already running".into());
        }
        let svc = ProxyService::new(core)
            .with_host_port(host.clone(), port)
            .with_auth_token(auth_token.clone());
        let circuits = svc.circuits.clone();
        let (tx, rx) = oneshot::channel::<()>();
        let handle = async_runtime::spawn(async move {
            svc.serve_with_shutdown(async move {
                let _ = rx.await;
            })
            .await
        });
        inner.shutdown = Some(tx);
        inner.handle = Some(handle);
        inner.port = Some(port);
        inner.host = Some(host);
        inner.auth_enabled = auth_token.is_some();
        inner.circuits = Some(circuits);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), String> {
        let (tx, handle) = {
            let mut inner = self.inner.lock().unwrap();
            match (inner.shutdown.take(), inner.handle.take()) {
                (Some(tx), Some(handle)) => (tx, handle),
                _ => return Ok(()), // 幂等：未运行直接返回
            }
        };
        let _ = tx.send(());
        let _ = handle.await;
        let mut inner = self.inner.lock().unwrap();
        inner.port = None;
        inner.host = None;
        inner.auth_enabled = false;
        inner.circuits = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_switch_core::store::secrets::MockStore;
    use agent_switch_core::Core;

    fn core() -> Core {
        let dir = tempfile::tempdir().unwrap();
        Core::with_paths(
            &dir.path().join("home"),
            &dir.path().join("data"),
            Arc::new(MockStore::default()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn start_stop_idempotent() {
        let ctl = ProxyCtl::default();
        assert!(!ctl.status().running);
        // 取空闲端口
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        ctl.start(core(), "127.0.0.1".into(), port, None).unwrap();
        // 给监听一点时间
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(ctl.status().running);
        // 重复启动被拒
        assert!(ctl.start(core(), "127.0.0.1".into(), port, None).is_err());
        ctl.stop().await.unwrap();
        assert!(!ctl.status().running);
        // 再次停止幂等
        ctl.stop().await.unwrap();
    }
}
