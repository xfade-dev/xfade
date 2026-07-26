use super::ProxyService;
use crate::service::Core;
use axum::{
    extract::Request,
    http::StatusCode,
    response::Response,
    routing::any,
    Router,
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

pub struct MockUpstream {
    pub url: String,
    responses: Arc<Mutex<Vec<(StatusCode, String)>>>,
    calls: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockUpstream {
    pub async fn spawn() -> Self {
        let responses = Arc::new(Mutex::new(Vec::<(StatusCode, String)>::new()));
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
                let (status, body) = if resps.is_empty() {
                    (StatusCode::OK, "{}".to_string())
                } else if resps.len() == 1 {
                    resps[0].clone()
                } else {
                    resps.remove(0)
                };
                Response::builder()
                    .status(status)
                    .body(axum::body::Body::from(body))
                    .unwrap()
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
        self.responses
            .lock()
            .unwrap()
            .push((StatusCode::from_u16(status).unwrap(), body.to_string()));
    }

    pub fn respond_sequence(&self, seq: Vec<(u16, &str)>) {
        let mut resps = self.responses.lock().unwrap();
        for (status, body) in seq {
            resps.push((StatusCode::from_u16(status).unwrap(), body.to_string()));
        }
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

pub fn test_core(dir: &tempfile::TempDir, providers: &[(&str, &str, &str)]) -> Core {
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let core = Core::with_paths(&home, &data, Box::new(crate::store::secrets::MockStore::default()))
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
