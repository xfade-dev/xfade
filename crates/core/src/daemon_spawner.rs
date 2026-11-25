use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::error::{CoreError, Result};

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeResult {
    Healthy,
    Conflict,
    Unreachable,
}

/// Probe whether an xfade gateway is listening on the given port.
/// Returns:
/// - `ProbeResult::Healthy` if the port responds with 200 and the xfade app fingerprint.
/// - `ProbeResult::Conflict` if the port is open but does not return the xfade fingerprint.
/// - `ProbeResult::Unreachable` if connection was refused or could not be established.
pub fn probe_health(port: u16, timeout: Duration) -> ProbeResult {
    match raw_get_health(port, timeout) {
        RawHealth::Unreachable => ProbeResult::Unreachable,
        RawHealth::Response(resp)
            if resp.starts_with("HTTP/1.1 200") || resp.starts_with("HTTP/1.0 200") =>
        {
            if resp.contains("\"app\":\"xfade\"") || resp.contains("\"app\": \"xfade\"") {
                ProbeResult::Healthy
            } else {
                ProbeResult::Conflict
            }
        }
        RawHealth::Response(_) => ProbeResult::Conflict,
    }
}

/// Fetch `GET /health` and return the daemon's reported `version`, if the
/// response is a 200 with a parseable `version` field. Older daemons that
/// predate the field yield `None` (treated as "unknown/out of date").
pub fn probe_version(port: u16, timeout: Duration) -> Option<String> {
    let resp = match raw_get_health(port, timeout) {
        RawHealth::Response(resp) => resp,
        RawHealth::Unreachable => return None,
    };
    let key = "\"version\"";
    let idx = resp.find(key)?;
    let after = &resp[idx + key.len()..];
    let colon = after.find(':')?;
    let after_colon = after[colon + 1..].trim_start();
    let s = after_colon.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(s[..end].to_string())
}

/// Outcome of a raw `GET /health`: either the port refused the connection
/// (Unreachable) or we received *some* response text (parse/verify elsewhere).
enum RawHealth {
    Unreachable,
    Response(String),
}

/// Raw `GET /health` over a blocking TCP socket. Shared by `probe_health` and
/// `probe_version` so both agree on the exact request shape.
fn raw_get_health(port: u16, timeout: Duration) -> RawHealth {
    let addr: SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => return RawHealth::Unreachable,
    };

    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(_) => return RawHealth::Unreachable,
    };

    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let req =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return RawHealth::Unreachable;
    }
    let _ = stream.flush();

    let mut buf = Vec::new();
    let mut temp = [0u8; 1024];
    loop {
        match stream.read(&mut temp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&temp[..n]);
                if buf.len() > 8192 {
                    break;
                }
            }
            Err(e) => {
                eprintln!("[debug probe_health read err] {e:?}");
                break;
            }
        }
    }

    RawHealth::Response(String::from_utf8_lossy(&buf).into_owned())
}

static DAEMON_EXE_OVERRIDE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Register an explicit `xfade` CLI executable path for background daemon
/// spawning (used by GUI hosts where `std::env::current_exe()` is `xfade-gui`).
pub fn set_daemon_exe(path: std::path::PathBuf) {
    let _ = DAEMON_EXE_OVERRIDE.set(path);
}

/// Resolve the executable used to spawn `xfade serve`.
pub fn daemon_exe() -> Result<std::path::PathBuf> {
    if let Some(path) = DAEMON_EXE_OVERRIDE.get() {
        return Ok(path.clone());
    }
    Ok(std::env::current_exe()?)
}

/// Ensure the xfade gateway is running on `port`.
/// If already running with valid fingerprint, returns `Ok(())`.
/// If occupied by a foreign server, returns `Err(CoreError::PortConflict(port))`.
/// If stopped, spawns `xfade serve --port <port>` and polls up to 1500ms for health.
pub fn ensure_gateway_running(port: u16) -> Result<()> {
    let initial_timeout = Duration::from_millis(200);
    match probe_health(port, initial_timeout) {
        ProbeResult::Healthy => {
            // Version handshake: if the running daemon is a different build than
            // this CLI (e.g. the binary was replaced but the process wasn't
            // restarted), kick the daemon so the serving process matches disk.
            // Skipped under XFADE_TEST_SKIP_SPAWN so tests never touch a real
            // launchd/systemd daemon that happens to be listening on the port.
            if std::env::var("XFADE_TEST_SKIP_SPAWN").is_err() {
                ensure_daemon_version(port);
            }
            return Ok(());
        }
        ProbeResult::Conflict => return Err(CoreError::PortConflict(port)),
        ProbeResult::Unreachable => {}
    }

    if std::env::var("XFADE_TEST_SKIP_SPAWN").is_ok() {
        return Ok(());
    }

    let current_exe = daemon_exe()?;
    let mut cmd = std::process::Command::new(current_exe);
    cmd.args(["serve", "--port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let _child = cmd.spawn()?;

    let start = Instant::now();
    let max_wait = Duration::from_millis(1500);
    while start.elapsed() < max_wait {
        std::thread::sleep(Duration::from_millis(50));
        match probe_health(port, Duration::from_millis(100)) {
            ProbeResult::Healthy => return Ok(()),
            ProbeResult::Conflict => return Err(CoreError::PortConflict(port)),
            ProbeResult::Unreachable => continue,
        }
    }

    Err(CoreError::Proxy(format!(
        "gateway auto-spawn timed out after 1500ms on port {port}"
    )))
}

/// Compare the running daemon's version against this build and restart it in
/// place on mismatch. Best-effort: a version that can't be read (older daemon)
/// or a failed restart only logs, never fails the caller.
fn ensure_daemon_version(port: u16) {
    let own = env!("CARGO_PKG_VERSION");
    match probe_version(port, Duration::from_millis(200)) {
        Some(v) if v == own => {}
        running => {
            let shown = running.as_deref().unwrap_or("(unknown)");
            eprintln!(
                "xfade: gateway daemon is v{shown} but this CLI is v{own}; restarting the daemon"
            );
            if let Err(e) = crate::daemon::restart() {
                eprintln!("xfade: daemon auto-restart failed ({e}); run `xfade serve restart`");
                return;
            }
            let start = Instant::now();
            let max_wait = Duration::from_millis(1500);
            while start.elapsed() < max_wait {
                std::thread::sleep(Duration::from_millis(50));
                if probe_health(port, Duration::from_millis(100)) == ProbeResult::Healthy {
                    return;
                }
            }
            eprintln!(
                "xfade: daemon did not report healthy after restart; check ~/.config/xfade/serve.log"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::testsupport::{spawn_service, test_core};
    use crate::proxy::ProxyService;
    use std::net::TcpListener;

    #[tokio::test]
    async fn test_health_check_returns_xfade_app_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let url = spawn_service(ProxyService::new(core)).await;
        let resp: serde_json::Value = reqwest::get(format!("{url}/health"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resp["app"], "xfade");
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_probe_version_reads_daemon_version() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let url = spawn_service(ProxyService::new(core)).await;
        let port: u16 = url.split(':').next_back().unwrap().parse().unwrap();
        assert_eq!(
            probe_version(port, Duration::from_millis(200)).as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ensure_gateway_running_healthy_service() {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core(&dir, &[]);
        let url = spawn_service(ProxyService::new(core)).await;
        let port: u16 = url.split(':').next_back().unwrap().parse().unwrap();

        assert_eq!(
            probe_health(port, Duration::from_millis(200)),
            ProbeResult::Healthy
        );
        assert!(ensure_gateway_running(port).is_ok());
    }

    #[test]
    fn test_probe_health_port_conflict() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let handle = std::thread::spawn(move || {
            while running_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 512];
                        let _ = stream.read(&mut buf);
                        let resp =
                            "HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nhello-foreign";
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        assert_eq!(
            probe_health(port, Duration::from_millis(200)),
            ProbeResult::Conflict
        );
        let err = ensure_gateway_running(port).unwrap_err();
        match err {
            CoreError::PortConflict(p) => assert_eq!(p, port),
            other => panic!("expected PortConflict, got {:?}", other),
        }

        running.store(false, Ordering::Relaxed);
        let _ = handle.join();
    }

    #[test]
    fn test_probe_health_unreachable() {
        // Use an unused port
        let port = 63821;
        assert_eq!(
            probe_health(port, Duration::from_millis(50)),
            ProbeResult::Unreachable
        );
    }

    #[test]
    fn daemon_exe_prefers_override_when_set() {
        let override_path = std::path::PathBuf::from("/tmp/custom-xfade-sidecar");
        set_daemon_exe(override_path.clone());
        assert_eq!(daemon_exe().unwrap(), override_path);
    }
}
