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
    let addr: SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => return ProbeResult::Unreachable,
    };

    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(_) => return ProbeResult::Unreachable,
    };

    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let req =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return ProbeResult::Unreachable;
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

    let resp_str = String::from_utf8_lossy(&buf);
    if resp_str.starts_with("HTTP/1.1 200") || resp_str.starts_with("HTTP/1.0 200") {
        if resp_str.contains("\"app\":\"xfade\"") || resp_str.contains("\"app\": \"xfade\"") {
            ProbeResult::Healthy
        } else {
            ProbeResult::Conflict
        }
    } else {
        ProbeResult::Conflict
    }
}

/// Ensure the xfade gateway is running on `port`.
/// If already running with valid fingerprint, returns `Ok(())`.
/// If occupied by a foreign server, returns `Err(CoreError::PortConflict(port))`.
/// If stopped, spawns `xfade serve --port <port>` and polls up to 1500ms for health.
pub fn ensure_gateway_running(port: u16) -> Result<()> {
    let initial_timeout = Duration::from_millis(200);
    match probe_health(port, initial_timeout) {
        ProbeResult::Healthy => return Ok(()),
        ProbeResult::Conflict => return Err(CoreError::PortConflict(port)),
        ProbeResult::Unreachable => {}
    }

    if std::env::var("XFADE_TEST_SKIP_SPAWN").is_ok() {
        return Ok(());
    }

    let current_exe = std::env::current_exe()?;
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
}
