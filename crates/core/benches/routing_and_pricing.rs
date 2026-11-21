use criterion::{criterion_group, criterion_main, Criterion};
use std::collections::BTreeMap;
use std::hint::black_box;
use xfade_core::models::ProviderProtocol;
use xfade_core::proxy::pricing::{
    calculate_cost_micros, canonical_alias, find_model_price, ModelPrice,
};
use xfade_core::proxy::robustness::SemanticDetector;
use xfade_core::router::decision::resolve_effective_model;

fn bench_pricing_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("pricing_lookup");

    group.bench_function("find_exact_match_claude_3_7_sonnet", |b| {
        b.iter(|| find_model_price(black_box("anthropic/claude-3-7-sonnet")))
    });

    group.bench_function("find_exact_match_gpt_4o", |b| {
        b.iter(|| find_model_price(black_box("gpt-4o")))
    });

    group.bench_function("find_prefix_match_dated_version", |b| {
        b.iter(|| find_model_price(black_box("claude-3-7-sonnet-20250219")))
    });

    group.bench_function("canonical_alias_resolution", |b| {
        b.iter(|| canonical_alias(black_box("claude-3-5-sonnet")))
    });

    let sample_price = ModelPrice {
        input_micros_per_m: 3_000_000,
        output_micros_per_m: 15_000_000,
        cache_read_micros_per_m: 300_000,
        cache_write_micros_per_m: 3_750_000,
    };

    group.bench_function("calculate_cost_micros_all_token_types", |b| {
        b.iter(|| {
            calculate_cost_micros(
                black_box(&sample_price),
                black_box(12_450),
                black_box(1_820),
                black_box(8_500),
                black_box(2_100),
            )
        })
    });

    group.finish();
}

fn bench_model_resolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("model_resolution");

    let mut client_mappings = BTreeMap::new();
    client_mappings.insert(
        "anthropic_direct".to_string(),
        "claude-3-7-sonnet".to_string(),
    );
    client_mappings.insert(
        "openrouter_relay".to_string(),
        "anthropic/claude-3.7-sonnet".to_string(),
    );
    client_mappings.insert("*".to_string(), "claude-3-5-sonnet".to_string());

    let mut route_models = BTreeMap::new();
    route_models.insert(
        "anthropic_direct".to_string(),
        "claude-3-5-sonnet".to_string(),
    );
    route_models.insert("openrouter_relay".to_string(), "openai/gpt-4o".to_string());

    group.bench_function("resolve_effective_model_explicit_hit", |b| {
        b.iter(|| {
            resolve_effective_model(
                black_box("anthropic_direct"),
                black_box(Some("sonnet")),
                black_box(Some(&client_mappings)),
                black_box(&route_models),
                black_box(Some("gpt-4o-mini")),
            )
        })
    });

    group.bench_function("resolve_effective_model_wildcard_hit", |b| {
        b.iter(|| {
            resolve_effective_model(
                black_box("unknown_route"),
                black_box(Some("sonnet")),
                black_box(Some(&client_mappings)),
                black_box(&route_models),
                black_box(Some("gpt-4o-mini")),
            )
        })
    });

    group.finish();
}

fn bench_semantic_detector(c: &mut Criterion) {
    let mut group = c.benchmark_group("semantic_detector");

    let sse_chat_delta = b"data: {\"id\":\"chatcmpl-123\",\"choices\":[{\"delta\":{\"content\":\"Hello world\"}}]}\n\n";
    let sse_anthropic_delta = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello world\"}}\n\n";
    let error_payload = b"{\"error\":{\"message\":\"Rate limit exceeded\",\"type\":\"rate_limit_error\",\"code\":\"rate_limit_exceeded\"}}";

    group.bench_function("inspect_chunk_chat_sse", |b| {
        b.iter(|| {
            SemanticDetector::inspect_chunk(
                black_box(sse_chat_delta),
                black_box(ProviderProtocol::Chat),
            )
        })
    });

    group.bench_function("inspect_chunk_messages_sse", |b| {
        b.iter(|| {
            SemanticDetector::inspect_chunk(
                black_box(sse_anthropic_delta),
                black_box(ProviderProtocol::Messages),
            )
        })
    });

    group.bench_function("is_error_payload_detection", |b| {
        b.iter(|| SemanticDetector::is_error_payload(black_box(error_payload)))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_pricing_lookup,
    bench_model_resolution,
    bench_semantic_detector
);
criterion_main!(benches);
