use axum::{
    body::Body, extract::Request, http::StatusCode, response::Response, routing::any, Router,
};
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::sync::Arc;
use tokio::net::TcpListener;
use xfade_core::models::{Provider, ProviderProtocol, ToolKind};
use xfade_core::proxy::ProxyService;
use xfade_core::service::Core;

async fn spawn_mock_upstream() -> String {
    let app = Router::new().fallback(any(|_req: Request| async move {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"id":"chatcmpl-bench","choices":[{"message":{"role":"assistant","content":"pong"}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
            ))
            .unwrap()
    }));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

async fn spawn_mock_streaming_upstream() -> String {
    let app = Router::new().fallback(any(|_req: Request| async move {
        let lines = vec![
            r#"data: {"id":"chatcmpl-bench","choices":[{"delta":{"content":"Hello"}}]}"#,
            r#"data: {"id":"chatcmpl-bench","choices":[{"delta":{"content":" world"}}]}"#,
            r#"data: [DONE]"#,
        ];
        let mut payload = Vec::new();
        for line in lines {
            payload.extend_from_slice(line.as_bytes());
            payload.extend_from_slice(b"\n\n");
        }
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(payload))
            .unwrap()
    }));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

async fn spawn_proxy_service(svc: ProxyService) -> String {
    let app = svc.build_router();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

fn setup_benchmark_environment(
    rt: &tokio::runtime::Runtime,
) -> (
    tempfile::TempDir,
    String, // direct upstream url
    String, // direct streaming url
    String, // proxy url
    String, // client token
    reqwest::Client,
) {
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let secrets = Arc::new(xfade_core::store::secrets::MockStore::default());
        let core = Core::with_paths(&home, &data, secrets.clone()).unwrap();

        // Spawn mock direct upstream
        let direct_url = spawn_mock_upstream().await;
        let direct_streaming_url = spawn_mock_streaming_upstream().await;

        // Register provider pointing to direct_url
        core.secrets().set("ref:bench_mock", "sk-mock-key").unwrap();
        let p = Provider::new("bench_mock", ToolKind::Aider, Some(direct_url.clone()))
            .with_protocol(ProviderProtocol::Chat);
        core.db().upsert(&p).unwrap();
        core.db()
            .set_routes(&["bench_mock".to_string()], None, "aider")
            .unwrap();

        // Spawn proxy service
        let svc = ProxyService::new(core).with_host_port("127.0.0.1", 0);
        let client_token = svc.token_manager.get_or_create(ToolKind::Aider).unwrap();
        let proxy_url = spawn_proxy_service(svc).await;

        let client = reqwest::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();

        (
            dir,
            direct_url,
            direct_streaming_url,
            proxy_url,
            client_token,
            client,
        )
    })
}

fn bench_proxy_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let (_dir, direct_url, _direct_stream_url, proxy_url, client_token, client) =
        setup_benchmark_environment(&rt);

    let chat_body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "ping"}],
        "stream": false
    })
    .to_string();

    let streaming_chat_body = serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "ping"}],
        "stream": true
    })
    .to_string();

    let mut group = c.benchmark_group("gateway_latency_overhead");
    group.sample_size(50);
    group.measurement_time(std::time::Duration::from_secs(4));

    // 1. Baseline: Direct upstream loopback roundtrip
    group.bench_function("direct_upstream_roundtrip", |b| {
        let client = client.clone();
        let url = format!("{direct_url}/v1/chat/completions");
        let body = chat_body.clone();
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            async move {
                let resp = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Authorization", "Bearer sk-direct")
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                let bytes = resp.bytes().await.unwrap();
                black_box(bytes);
            }
        });
    });

    // 2. Gateway Proxy Forwarding: Token Auth + Route Resolve + Header Injection + Forwarding + Cost Tracking
    group.bench_function("proxied_gateway_roundtrip", |b| {
        let client = client.clone();
        let url = format!("{proxy_url}/v1/chat/completions");
        let body = chat_body.clone();
        let auth_header = format!("Bearer {client_token}");
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            let auth_header = auth_header.clone();
            async move {
                let resp = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Authorization", &auth_header)
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                let bytes = resp.bytes().await.unwrap();
                black_box(bytes);
            }
        });
    });

    // 3. Streaming Gateway Forwarding with Commit Gate
    group.bench_function("proxied_gateway_streaming", |b| {
        let client = client.clone();
        let url = format!("{proxy_url}/v1/chat/completions");
        let body = streaming_chat_body.clone();
        let auth_header = format!("Bearer {client_token}");
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            let auth_header = auth_header.clone();
            async move {
                let resp = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Authorization", &auth_header)
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                let bytes = resp.bytes().await.unwrap();
                black_box(bytes);
            }
        });
    });

    // 4. Aggregated Models Endpoint
    group.bench_function("proxied_models_endpoint", |b| {
        let client = client.clone();
        let url = format!("{proxy_url}/v1/models");
        let auth_header = format!("Bearer {client_token}");
        b.to_async(&rt).iter(|| {
            let client = client.clone();
            let url = url.clone();
            let auth_header = auth_header.clone();
            async move {
                let resp = client
                    .get(&url)
                    .header("Authorization", &auth_header)
                    .send()
                    .await
                    .unwrap();
                let bytes = resp.bytes().await.unwrap();
                black_box(bytes);
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_proxy_latency);
criterion_main!(benches);
