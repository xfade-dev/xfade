use super::ProxyService;
use crate::service::Core;
use axum::{
    body::Body, extract::Request, http::StatusCode, response::Response, routing::any, Router,
};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub authorization: String,
    pub body: String,
}

/// Mock upstream response descriptor.
#[derive(Clone)]
enum MockResponse {
    /// Non-streaming: status + body bytes.
    Plain { status: StatusCode, body: String },
    /// Streaming: status + content-type text/event-stream + lines (each terminated with `\n\n`).
    Sse {
        status: StatusCode,
        lines: Vec<String>,
    },
    /// Streaming that emits `lines` then errors mid-stream (to simulate an
    /// interrupted upstream connection). Used by the interrupted-stream
    /// regression test.
    SseError {
        status: StatusCode,
        lines: Vec<String>,
    },
}

pub struct MockUpstream {
    pub url: String,
    responses: Arc<Mutex<Vec<MockResponse>>>,
    calls: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockUpstream {
    pub async fn spawn() -> Self {
        let responses = Arc::new(Mutex::new(Vec::<MockResponse>::new()));
        let calls = Arc::new(Mutex::new(Vec::<RecordedRequest>::new()));

        let r = responses.clone();
        let c = calls.clone();
        let app = Router::new().fallback(any(move |req: Request| {
            let r = r.clone();
            let c = c.clone();
            async move {
                let (parts, body) = req.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let recorded = RecordedRequest {
                    method: parts.method.to_string(),
                    path: parts.uri.path().to_string(),
                    authorization: parts
                        .headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string(),
                    body: String::from_utf8_lossy(&bytes).to_string(),
                };
                c.lock().unwrap().push(recorded);

                let mut resps = r.lock().unwrap();
                let resp = if resps.is_empty() {
                    MockResponse::Plain {
                        status: StatusCode::OK,
                        body: "{}".to_string(),
                    }
                } else if resps.len() == 1 {
                    resps[0].clone()
                } else {
                    resps.remove(0)
                };
                drop(resps);

                match resp {
                    MockResponse::Plain { status, body } => Response::builder()
                        .status(status)
                        .body(Body::from(body))
                        .unwrap(),
                    MockResponse::Sse { status, lines } => {
                        // Build a single body chunk where each line is terminated by `\n\n`,
                        // then close the stream so the client doesn't hang.
                        let mut payload = Vec::new();
                        for line in &lines {
                            payload.extend_from_slice(line.as_bytes());
                            payload.extend_from_slice(b"\n\n");
                        }
                        Response::builder()
                            .status(status)
                            .header("content-type", "text/event-stream")
                            .body(Body::from(payload))
                            .unwrap()
                    }
                    MockResponse::SseError { status, lines } => {
                        // Emit each line as its own chunk (terminated by `\n\n`),
                        // then yield an error to simulate a mid-stream failure.
                        // A short sleep before the terminal error lets axum flush
                        // the response headers + chunks so reqwest's `send()`
                        // succeeds; only `bytes()` then surfaces the error.
                        let owned_lines: Vec<String> = lines.clone();
                        let s = futures::stream::unfold(
                            (owned_lines, 0usize, false),
                            |(lines, idx, yielded_err)| async move {
                                if idx < lines.len() {
                                    let mut payload = lines[idx].as_bytes().to_vec();
                                    payload.extend_from_slice(b"\n\n");
                                    Some((
                                        Ok::<axum::body::Bytes, std::io::Error>(
                                            axum::body::Bytes::from(payload),
                                        ),
                                        (lines, idx + 1, false),
                                    ))
                                } else if !yielded_err {
                                    // Give axum time to flush headers + chunks.
                                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                    Some((
                                        Err(std::io::Error::new(
                                            std::io::ErrorKind::ConnectionAborted,
                                            "upstream stream interrupted",
                                        )),
                                        (lines, idx, true),
                                    ))
                                } else {
                                    None
                                }
                            },
                        );
                        Response::builder()
                            .status(status)
                            .header("content-type", "text/event-stream")
                            .body(Body::from_stream(s))
                            .unwrap()
                    }
                }
            }
        }));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            url,
            responses,
            calls,
        }
    }

    pub fn url(&self) -> String {
        self.url.clone()
    }

    pub fn respond_with(&self, status: u16, body: &str) {
        self.responses.lock().unwrap().push(MockResponse::Plain {
            status: StatusCode::from_u16(status).unwrap(),
            body: body.to_string(),
        });
    }

    pub fn respond_sequence(&self, seq: Vec<(u16, &str)>) {
        let mut resps = self.responses.lock().unwrap();
        for (status, body) in seq {
            resps.push(MockResponse::Plain {
                status: StatusCode::from_u16(status).unwrap(),
                body: body.to_string(),
            });
        }
    }

    /// Queue a streaming text/event-stream response. Each line is written as
    /// `line\n\n` and the connection is closed after the last line so that
    /// `reqwest`'s response body consumption terminates cleanly.
    pub fn respond_sse(&self, lines: Vec<&str>) {
        self.responses.lock().unwrap().push(MockResponse::Sse {
            status: StatusCode::OK,
            lines: lines.into_iter().map(String::from).collect(),
        });
    }

    /// Queue a streaming text/event-stream response that emits `lines` then
    /// errors mid-stream, simulating an interrupted upstream connection.
    pub fn respond_sse_error(&self, lines: Vec<&str>) {
        self.responses.lock().unwrap().push(MockResponse::SseError {
            status: StatusCode::OK,
            lines: lines.into_iter().map(String::from).collect(),
        });
    }

    pub async fn last_request(&self) -> RecordedRequest {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("no request recorded yet")
    }

    pub async fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

pub async fn spawn_service(svc: ProxyService) -> String {
    let app = svc.build_router();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

pub async fn http_post_json(url: &str, path: &str, body: &str, auth: &str) -> (u16, String) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}{path}"))
        .header("Content-Type", "application/json")
        .header("Authorization", auth)
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (status, text)
}

/// Like `http_post_json` but tolerates mid-stream body errors (returns
/// whatever bytes were received before the error). Used by the
/// interrupted-stream regression test.
pub async fn http_post_json_lossy(url: &str, path: &str, body: &str, auth: &str) -> (u16, String) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}{path}"))
        .header("Content-Type", "application/json")
        .header("Authorization", auth)
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    // Use bytes() which may return partial bytes on error, then fall back to text.
    let text = match resp.bytes().await {
        Ok(b) => String::from_utf8_lossy(&b).to_string(),
        Err(_) => String::new(),
    };
    (status, text)
}

pub fn test_core(dir: &tempfile::TempDir, providers: &[(&str, &str, &str)]) -> Core {
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(
        &home,
        &data,
        Box::new(crate::store::secrets::MockStore::default()),
    )
    .unwrap();
    for (id, base_url, key) in providers {
        let p = crate::models::Provider::new(
            *id,
            crate::models::ToolKind::Codex,
            Some(base_url.to_string()),
        );
        core.add_provider(p, Some(key)).unwrap();
    }
    core
}
