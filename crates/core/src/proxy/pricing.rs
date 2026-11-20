//! Token pricing catalog and cost estimation for supported models.

use crate::store::db::Database;

/// Per-million token rates in micro-dollars ($1.00 USD = 1,000,000 micros).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPrice {
    /// USD micros per 1,000,000 input (uncached prompt) tokens
    pub input_micros_per_m: i64,
    /// USD micros per 1,000,000 output (completion) tokens
    pub output_micros_per_m: i64,
    /// USD micros per 1,000,000 cached input read tokens
    pub cache_read_micros_per_m: i64,
    /// USD micros per 1,000,000 cache creation/write tokens
    pub cache_write_micros_per_m: i64,
}

struct PriceEntry {
    prefix: &'static str,
    price: ModelPrice,
}

static CATALOG: &[PriceEntry] = &[
    // Anthropic flagship models (2026 Claude 5.5 series)
    PriceEntry {
        prefix: "claude-sonnet-5.5",
        price: ModelPrice {
            input_micros_per_m: 2_000_000,       // $2.00 / 1M
            output_micros_per_m: 10_000_000,     // $10.00 / 1M
            cache_read_micros_per_m: 100_000,    // $0.10 / 1M
            cache_write_micros_per_m: 2_500_000, // $2.50 / 1M
        },
    },
    PriceEntry {
        prefix: "claude-opus-5.5",
        price: ModelPrice {
            input_micros_per_m: 4_000_000,       // $4.00 / 1M
            output_micros_per_m: 20_000_000,     // $20.00 / 1M
            cache_read_micros_per_m: 200_000,    // $0.20 / 1M
            cache_write_micros_per_m: 5_000_000, // $5.00 / 1M
        },
    },
    PriceEntry {
        prefix: "claude-haiku-5.5",
        price: ModelPrice {
            input_micros_per_m: 100_000,       // $0.10 / 1M
            output_micros_per_m: 500_000,      // $0.50 / 1M
            cache_read_micros_per_m: 10_000,   // $0.01 / 1M
            cache_write_micros_per_m: 125_000, // $0.125 / 1M
        },
    },
    PriceEntry {
        prefix: "claude-fable-5.2",
        price: ModelPrice {
            input_micros_per_m: 8_000_000,        // $8.00 / 1M
            output_micros_per_m: 40_000_000,      // $40.00 / 1M
            cache_read_micros_per_m: 200_000,     // $0.20 / 1M
            cache_write_micros_per_m: 10_000_000, // $10.00 / 1M
        },
    },
    // Anthropic 3.x series
    PriceEntry {
        prefix: "claude-3-7-sonnet",
        price: ModelPrice {
            input_micros_per_m: 3_000_000,       // $3.00 / 1M
            output_micros_per_m: 15_000_000,     // $15.00 / 1M
            cache_read_micros_per_m: 300_000,    // $0.30 / 1M (90% discount)
            cache_write_micros_per_m: 3_750_000, // $3.75 / 1M (+25%)
        },
    },
    PriceEntry {
        prefix: "claude-3-5-sonnet",
        price: ModelPrice {
            input_micros_per_m: 3_000_000,
            output_micros_per_m: 15_000_000,
            cache_read_micros_per_m: 300_000,
            cache_write_micros_per_m: 3_750_000,
        },
    },
    PriceEntry {
        prefix: "claude-3-5-haiku",
        price: ModelPrice {
            input_micros_per_m: 800_000,         // $0.80 / 1M
            output_micros_per_m: 4_000_000,      // $4.00 / 1M
            cache_read_micros_per_m: 80_000,     // $0.08 / 1M
            cache_write_micros_per_m: 1_000_000, // $1.00 / 1M
        },
    },
    PriceEntry {
        prefix: "claude-3-haiku",
        price: ModelPrice {
            input_micros_per_m: 250_000,    // $0.25 / 1M
            output_micros_per_m: 1_250_000, // $1.25 / 1M
            cache_read_micros_per_m: 25_000,
            cache_write_micros_per_m: 312_500,
        },
    },
    PriceEntry {
        prefix: "claude-3-opus",
        price: ModelPrice {
            input_micros_per_m: 15_000_000,       // $15.00 / 1M
            output_micros_per_m: 75_000_000,      // $75.00 / 1M
            cache_read_micros_per_m: 1_500_000,   // $1.50 / 1M
            cache_write_micros_per_m: 18_750_000, // $18.75 / 1M
        },
    },
    // OpenAI flagship models (2026 GPT-6.x series)
    PriceEntry {
        prefix: "gpt-6.2-astra",
        price: ModelPrice {
            input_micros_per_m: 8_000_000,        // $8.00 / 1M
            output_micros_per_m: 40_000_000,      // $40.00 / 1M
            cache_read_micros_per_m: 400_000,     // $0.40 / 1M
            cache_write_micros_per_m: 10_000_000, // $10.00 / 1M
        },
    },
    PriceEntry {
        prefix: "gpt-6.2-sol",
        price: ModelPrice {
            input_micros_per_m: 2_000_000,       // $2.00 / 1M
            output_micros_per_m: 10_000_000,     // $10.00 / 1M
            cache_read_micros_per_m: 100_000,    // $0.10 / 1M
            cache_write_micros_per_m: 2_500_000, // $2.50 / 1M
        },
    },
    PriceEntry {
        prefix: "gpt-6.1-terra",
        price: ModelPrice {
            input_micros_per_m: 1_000_000,       // $1.00 / 1M
            output_micros_per_m: 5_000_000,      // $5.00 / 1M
            cache_read_micros_per_m: 50_000,     // $0.05 / 1M
            cache_write_micros_per_m: 1_250_000, // $1.25 / 1M
        },
    },
    PriceEntry {
        prefix: "gpt-6-luna",
        price: ModelPrice {
            input_micros_per_m: 100_000,       // $0.10 / 1M
            output_micros_per_m: 500_000,      // $0.50 / 1M
            cache_read_micros_per_m: 10_000,   // $0.01 / 1M
            cache_write_micros_per_m: 125_000, // $0.125 / 1M
        },
    },
    // OpenAI established models
    PriceEntry {
        prefix: "gpt-4o-mini",
        price: ModelPrice {
            input_micros_per_m: 150_000,     // $0.15 / 1M
            output_micros_per_m: 600_000,    // $0.60 / 1M
            cache_read_micros_per_m: 75_000, // $0.075 / 1M
            cache_write_micros_per_m: 150_000,
        },
    },
    PriceEntry {
        prefix: "gpt-4o",
        price: ModelPrice {
            input_micros_per_m: 2_500_000,      // $2.50 / 1M
            output_micros_per_m: 10_000_000,    // $10.00 / 1M
            cache_read_micros_per_m: 1_250_000, // $1.25 / 1M
            cache_write_micros_per_m: 2_500_000,
        },
    },
    PriceEntry {
        prefix: "o1-mini",
        price: ModelPrice {
            input_micros_per_m: 1_100_000,
            output_micros_per_m: 4_400_000,
            cache_read_micros_per_m: 550_000,
            cache_write_micros_per_m: 1_100_000,
        },
    },
    PriceEntry {
        prefix: "o1",
        price: ModelPrice {
            input_micros_per_m: 15_000_000,
            output_micros_per_m: 60_000_000,
            cache_read_micros_per_m: 7_500_000,
            cache_write_micros_per_m: 15_000_000,
        },
    },
    PriceEntry {
        prefix: "o3-mini",
        price: ModelPrice {
            input_micros_per_m: 1_100_000,
            output_micros_per_m: 4_400_000,
            cache_read_micros_per_m: 550_000,
            cache_write_micros_per_m: 1_100_000,
        },
    },
    // Google Gemini 4 & 2.x models
    PriceEntry {
        prefix: "gemini-4-neon",
        price: ModelPrice {
            input_micros_per_m: 3_000_000,    // $3.00 / 1M
            output_micros_per_m: 15_000_000,  // $15.00 / 1M
            cache_read_micros_per_m: 150_000, // $0.15 / 1M
            cache_write_micros_per_m: 83_000, // $0.083 / 1M
        },
    },
    PriceEntry {
        prefix: "gemini-4-hydrogen",
        price: ModelPrice {
            input_micros_per_m: 1_500_000,   // $1.50 / 1M
            output_micros_per_m: 7_500_000,  // $7.50 / 1M
            cache_read_micros_per_m: 75_000, // $0.075 / 1M
            cache_write_micros_per_m: 41_500,
        },
    },
    PriceEntry {
        prefix: "gemini-4-carbon",
        price: ModelPrice {
            input_micros_per_m: 2_000_000,    // $2.00 / 1M
            output_micros_per_m: 10_000_000,  // $10.00 / 1M
            cache_read_micros_per_m: 100_000, // $0.10 / 1M
            cache_write_micros_per_m: 55_000,
        },
    },
    PriceEntry {
        prefix: "gemini-2.0-flash",
        price: ModelPrice {
            input_micros_per_m: 100_000,  // $0.10 / 1M
            output_micros_per_m: 400_000, // $0.40 / 1M
            cache_read_micros_per_m: 25_000,
            cache_write_micros_per_m: 100_000,
        },
    },
    PriceEntry {
        prefix: "gemini-1.5-flash",
        price: ModelPrice {
            input_micros_per_m: 75_000,   // $0.075 / 1M
            output_micros_per_m: 300_000, // $0.30 / 1M
            cache_read_micros_per_m: 18_750,
            cache_write_micros_per_m: 75_000,
        },
    },
    PriceEntry {
        prefix: "gemini-1.5-pro",
        price: ModelPrice {
            input_micros_per_m: 1_250_000,  // $1.25 / 1M
            output_micros_per_m: 5_000_000, // $5.00 / 1M
            cache_read_micros_per_m: 312_500,
            cache_write_micros_per_m: 1_250_000,
        },
    },
    // DeepSeek V4 & V3 series
    PriceEntry {
        prefix: "deepseek-v4.1-flash",
        price: ModelPrice {
            input_micros_per_m: 36_000,      // $0.036 / 1M
            output_micros_per_m: 500_000,    // $0.50 / 1M
            cache_read_micros_per_m: 10_000, // $0.01 / 1M
            cache_write_micros_per_m: 36_000,
        },
    },
    PriceEntry {
        prefix: "deepseek-v4-pro",
        price: ModelPrice {
            input_micros_per_m: 1_320_000,   // $1.32 / 1M
            output_micros_per_m: 3_960_000,  // $3.96 / 1M
            cache_read_micros_per_m: 44_000, // $0.044 / 1M
            cache_write_micros_per_m: 1_320_000,
        },
    },
    PriceEntry {
        prefix: "deepseek-v3.2",
        price: ModelPrice {
            input_micros_per_m: 260_000,     // $0.26 / 1M
            output_micros_per_m: 380_000,    // $0.38 / 1M
            cache_read_micros_per_m: 26_000, // $0.026 / 1M
            cache_write_micros_per_m: 260_000,
        },
    },
    PriceEntry {
        prefix: "deepseek-chat",
        price: ModelPrice {
            input_micros_per_m: 140_000,     // $0.14 / 1M
            output_micros_per_m: 280_000,    // $0.28 / 1M
            cache_read_micros_per_m: 14_000, // $0.014 / 1M (cache hit)
            cache_write_micros_per_m: 140_000,
        },
    },
    PriceEntry {
        prefix: "deepseek-v3",
        price: ModelPrice {
            input_micros_per_m: 140_000,
            output_micros_per_m: 280_000,
            cache_read_micros_per_m: 14_000,
            cache_write_micros_per_m: 140_000,
        },
    },
    PriceEntry {
        prefix: "deepseek-reasoner",
        price: ModelPrice {
            input_micros_per_m: 550_000,      // $0.55 / 1M
            output_micros_per_m: 2_190_000,   // $2.19 / 1M
            cache_read_micros_per_m: 140_000, // $0.14 / 1M
            cache_write_micros_per_m: 550_000,
        },
    },
    PriceEntry {
        prefix: "deepseek-r1",
        price: ModelPrice {
            input_micros_per_m: 550_000,
            output_micros_per_m: 2_190_000,
            cache_read_micros_per_m: 140_000,
            cache_write_micros_per_m: 550_000,
        },
    },
    // Alibaba Qwen series
    PriceEntry {
        prefix: "qwen4-max",
        price: ModelPrice {
            input_micros_per_m: 1_425_000,       // $1.425 / 1M
            output_micros_per_m: 4_275_000,      // $4.275 / 1M
            cache_read_micros_per_m: 71_000,     // $0.071 / 1M
            cache_write_micros_per_m: 1_781_000, // $1.781 / 1M
        },
    },
    PriceEntry {
        prefix: "qwen3.8-flash",
        price: ModelPrice {
            input_micros_per_m: 150_000,  // $0.15 / 1M
            output_micros_per_m: 470_000, // $0.47 / 1M
            cache_read_micros_per_m: 15_000,
            cache_write_micros_per_m: 150_000,
        },
    },
    PriceEntry {
        prefix: "qwen3.7-max",
        price: ModelPrice {
            input_micros_per_m: 1_475_000,  // $1.475 / 1M
            output_micros_per_m: 4_425_000, // $4.425 / 1M
            cache_read_micros_per_m: 74_000,
            cache_write_micros_per_m: 1_843_000,
        },
    },
    PriceEntry {
        prefix: "qwen-2.5-coder",
        price: ModelPrice {
            input_micros_per_m: 660_000,    // $0.66 / 1M
            output_micros_per_m: 1_000_000, // $1.00 / 1M
            cache_read_micros_per_m: 66_000,
            cache_write_micros_per_m: 660_000,
        },
    },
    // Moonshot Kimi series
    PriceEntry {
        prefix: "kimi-k3.1-pro",
        price: ModelPrice {
            input_micros_per_m: 253_000,      // $0.253 / 1M
            output_micros_per_m: 8_800_000,   // $8.80 / 1M
            cache_read_micros_per_m: 180_000, // $0.18 / 1M
            cache_write_micros_per_m: 253_000,
        },
    },
    PriceEntry {
        prefix: "kimi-k2.7-code",
        price: ModelPrice {
            input_micros_per_m: 600_000,    // $0.60 / 1M
            output_micros_per_m: 3_000_000, // $3.00 / 1M
            cache_read_micros_per_m: 60_000,
            cache_write_micros_per_m: 600_000,
        },
    },
    PriceEntry {
        prefix: "kimi-k2.5",
        price: ModelPrice {
            input_micros_per_m: 500_000,    // $0.50 / 1M
            output_micros_per_m: 2_500_000, // $2.50 / 1M
            cache_read_micros_per_m: 50_000,
            cache_write_micros_per_m: 500_000,
        },
    },
    // Zhipu GLM series
    PriceEntry {
        prefix: "glm-5.3-flash",
        price: ModelPrice {
            input_micros_per_m: 40_000,  // $0.04 / 1M
            output_micros_per_m: 80_000, // $0.08 / 1M
            cache_read_micros_per_m: 30_000,
            cache_write_micros_per_m: 40_000,
        },
    },
    PriceEntry {
        prefix: "glm-5.3",
        price: ModelPrice {
            input_micros_per_m: 70_000,     // $0.07 / 1M
            output_micros_per_m: 1_400_000, // $1.40 / 1M
            cache_read_micros_per_m: 35_000,
            cache_write_micros_per_m: 70_000,
        },
    },
    PriceEntry {
        prefix: "glm-5.2",
        price: ModelPrice {
            input_micros_per_m: 620_000,    // $0.62 / 1M
            output_micros_per_m: 2_420_000, // $2.42 / 1M
            cache_read_micros_per_m: 62_000,
            cache_write_micros_per_m: 620_000,
        },
    },
    PriceEntry {
        prefix: "glm-4-plus",
        price: ModelPrice {
            input_micros_per_m: 1_400_000,  // $1.40 / 1M
            output_micros_per_m: 1_400_000, // $1.40 / 1M
            cache_read_micros_per_m: 140_000,
            cache_write_micros_per_m: 1_400_000,
        },
    },
    // Mistral series
    PriceEntry {
        prefix: "pixtral-large-2502",
        price: ModelPrice {
            input_micros_per_m: 2_000_000,  // $2.00 / 1M
            output_micros_per_m: 6_000_000, // $6.00 / 1M
            cache_read_micros_per_m: 200_000,
            cache_write_micros_per_m: 2_000_000,
        },
    },
    PriceEntry {
        prefix: "mistral-large-4-0",
        price: ModelPrice {
            input_micros_per_m: 700_000,    // $0.70 / 1M
            output_micros_per_m: 2_100_000, // $2.10 / 1M
            cache_read_micros_per_m: 70_000,
            cache_write_micros_per_m: 700_000,
        },
    },
    PriceEntry {
        prefix: "devstral-2512",
        price: ModelPrice {
            input_micros_per_m: 400_000,    // $0.40 / 1M
            output_micros_per_m: 2_000_000, // $2.00 / 1M
            cache_read_micros_per_m: 40_000,
            cache_write_micros_per_m: 400_000,
        },
    },
];

/// Enumerate all static entries defined in the built-in catalog.
pub fn list_builtin_catalog() -> Vec<(&'static str, ModelPrice)> {
    CATALOG.iter().map(|e| (e.prefix, e.price)).collect()
}

/// Normalize common model aliases to their canonical model names.
pub fn canonical_alias(raw_name: &str) -> &str {
    match raw_name {
        "sonnet" => "claude-sonnet-5.5",
        "opus" => "claude-opus-5.5",
        "haiku" => "claude-haiku-5.5",
        "astra" => "gpt-6.2-astra",
        "sol" => "gpt-6.2-sol",
        "terra" => "gpt-6.1-terra",
        "luna" => "gpt-6-luna",
        "4o" => "gpt-4o",
        "4o-mini" => "gpt-4o-mini",
        "neon" => "gemini-4-neon",
        "hydrogen" => "gemini-4-hydrogen",
        "carbon" => "gemini-4-carbon",
        "v4-flash" | "deepseek-v4" => "deepseek-v4.1-flash",
        "v4-pro" => "deepseek-v4-pro",
        "qwen4" => "qwen4-max",
        "qwen-coder" => "qwen-2.5-coder",
        "kimi-k3" => "kimi-k3.1-pro",
        "kimi-code" => "kimi-k2.7-code",
        "kimi" => "kimi-k2.5",
        "glm-5-flash" => "glm-5.3-flash",
        "glm-5" => "glm-5.3",
        "devstral" => "devstral-2512",
        other => other,
    }
}

/// Find the model price entry using prefix and alias normalization.
pub fn find_model_price(model_name: &str) -> Option<&'static ModelPrice> {
    let trimmed = model_name.trim().to_lowercase();
    // Strip multi-tier provider prefixes if present (e.g. "openrouter/anthropic/claude-3-7-sonnet" -> "claude-3-7-sonnet")
    let raw_name = if let Some((_, rest)) = trimmed.rsplit_once('/') {
        rest
    } else {
        &trimmed
    };

    let canonical_name = canonical_alias(raw_name);

    // Find longest matching prefix
    let mut best_match: Option<(&'static str, &'static ModelPrice)> = None;
    for entry in CATALOG {
        if canonical_name.starts_with(entry.prefix) {
            match best_match {
                Some((prev_prefix, _)) => {
                    if entry.prefix.len() > prev_prefix.len() {
                        best_match = Some((entry.prefix, &entry.price));
                    }
                }
                None => {
                    best_match = Some((entry.prefix, &entry.price));
                }
            }
        }
    }

    best_match.map(|(_, price)| price)
}

/// Calculate the estimated cost in micro-dollars ($1.00 = 1,000,000 micros).
pub fn calculate_cost_micros(
    price: &ModelPrice,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_prompt_tokens: i64,
    cache_write_tokens: i64,
) -> i64 {
    let prompt_tokens = prompt_tokens.max(0);
    let completion_tokens = completion_tokens.max(0);
    let cached_prompt_tokens = cached_prompt_tokens.max(0);
    let cache_write_tokens = cache_write_tokens.max(0);

    // Uncached prompt tokens: total prompt minus cached read tokens minus cache write tokens.
    let uncached_prompt =
        (prompt_tokens - cached_prompt_tokens - cache_write_tokens).max(0) as i128;
    let cached_prompt = cached_prompt_tokens as i128;
    let completion = completion_tokens as i128;
    let cache_write = cache_write_tokens as i128;

    let cost_input = uncached_prompt * (price.input_micros_per_m as i128);
    let cost_cache_read = cached_prompt * (price.cache_read_micros_per_m as i128);
    let cost_output = completion * (price.output_micros_per_m as i128);
    let cost_cache_write = cache_write * (price.cache_write_micros_per_m as i128);

    let total = cost_input + cost_cache_read + cost_output + cost_cache_write;
    ((total + 500_000) / 1_000_000) as i64
}

/// Estimate cost using the three-tier resolution hierarchy:
/// 1. User custom override in database (`source = 'custom'`)
/// 2. Live synced model pricing in database (`source = 'synced'`)
/// 3. Built-in fallback catalog (`CATALOG`)
pub fn estimate_cost_with_db(
    db: Option<&Database>,
    model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_prompt_tokens: i64,
    cache_write_tokens: i64,
) -> i64 {
    let Some(m) = model else { return 0 };
    let trimmed = m.trim().to_lowercase();
    let raw = trimmed.rsplit_once('/').map(|(_, r)| r).unwrap_or(&trimmed);
    let canonical = canonical_alias(raw);

    // 1 & 2: Dynamic DB lookup (custom then synced)
    if let Some(db_conn) = db {
        if let Ok(Some(dynamic)) = db_conn.find_dynamic_model_price(&trimmed, Some(canonical)) {
            let price = ModelPrice {
                input_micros_per_m: dynamic.input_micros_per_m,
                output_micros_per_m: dynamic.output_micros_per_m,
                cache_read_micros_per_m: dynamic.cache_read_micros_per_m,
                cache_write_micros_per_m: dynamic.cache_write_micros_per_m,
            };
            return calculate_cost_micros(
                &price,
                prompt_tokens,
                completion_tokens,
                cached_prompt_tokens,
                cache_write_tokens,
            );
        }
    }

    // 3: Built-in fallback catalog
    if let Some(price) = find_model_price(m) {
        calculate_cost_micros(
            price,
            prompt_tokens,
            completion_tokens,
            cached_prompt_tokens,
            cache_write_tokens,
        )
    } else {
        0
    }
}

/// Convenience helper to estimate cost for a named model, defaulting to 0 if unknown.
pub fn estimate_cost(
    model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_prompt_tokens: i64,
    cache_write_tokens: i64,
) -> i64 {
    estimate_cost_with_db(
        None,
        model,
        prompt_tokens,
        completion_tokens,
        cached_prompt_tokens,
        cache_write_tokens,
    )
}

/// Format micro-dollars into a human-readable USD string (e.g. "$0.0084", "$1.2450").
pub fn format_cost(micros: i64) -> String {
    if micros <= 0 {
        return "$0.0000".to_string();
    }
    if micros < 50 {
        return "< $0.0001".to_string();
    }
    let dollars = micros as f64 / 1_000_000.0;
    format!("${:.4}", dollars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_lookup_exact_and_prefix() {
        let p_sonnet =
            find_model_price("claude-3-7-sonnet").expect("should find claude-3-7-sonnet");
        assert_eq!(p_sonnet.input_micros_per_m, 3_000_000);
        assert_eq!(p_sonnet.output_micros_per_m, 15_000_000);

        // Date-suffixed prefix
        let p_dated = find_model_price("claude-3-7-sonnet-20250219").expect("should match prefix");
        assert_eq!(p_dated.input_micros_per_m, 3_000_000);

        // Provider-scoped prefix
        let p_scoped =
            find_model_price("anthropic/claude-3-5-sonnet").expect("should match stripped prefix");
        assert_eq!(p_scoped.input_micros_per_m, 3_000_000);

        // Multi-tier provider-scoped prefix
        let p_multitier = find_model_price("openrouter/anthropic/claude-3-7-sonnet")
            .expect("should strip multi-tier prefix");
        assert_eq!(p_multitier.input_micros_per_m, 3_000_000);

        // Common model aliases
        let p_alias = find_model_price("sonnet").expect("should resolve sonnet alias");
        assert_eq!(p_alias.input_micros_per_m, 2_000_000);
        let p_4o_alias = find_model_price("4o").expect("should resolve 4o alias");
        assert_eq!(p_4o_alias.input_micros_per_m, 2_500_000);

        // GPT-4o
        let p_4o = find_model_price("gpt-4o-2024-08-06").expect("should match gpt-4o");
        assert_eq!(p_4o.input_micros_per_m, 2_500_000);
        assert_eq!(p_4o.output_micros_per_m, 10_000_000);

        // Unknown model
        assert!(find_model_price("my-custom-unregistered-model").is_none());
    }

    #[test]
    fn test_calculate_cost_exact() {
        let p_sonnet = find_model_price("claude-3-7-sonnet").unwrap();
        // 1,000,000 prompt + 1,000,000 completion -> $3.00 + $15.00 = $18.00 = 18,000,000 micros
        let cost = calculate_cost_micros(p_sonnet, 1_000_000, 1_000_000, 0, 0);
        assert_eq!(cost, 18_000_000);

        // 10,000 prompt + 2,000 completion -> 10k * $3/M + 2k * $15/M = $0.03 + $0.03 = $0.06 = 60,000 micros
        let cost_small = calculate_cost_micros(p_sonnet, 10_000, 2_000, 0, 0);
        assert_eq!(cost_small, 60_000);
    }

    #[test]
    fn test_calculate_cost_with_cache_discount() {
        let p_sonnet = find_model_price("claude-3-7-sonnet").unwrap();
        // 100,000 total prompt, 80,000 cached (read), 20,000 uncached. 5,000 completion.
        // Uncached: 20,000 * 3,000,000 / 1M = 60,000 micros ($0.06)
        // Cached read: 80,000 * 300,000 / 1M = 24,000 micros ($0.024)
        // Completion: 5,000 * 15,000,000 / 1M = 75,000 micros ($0.075)
        // Total: 60,000 + 24,000 + 75,000 = 159,000 micros ($0.159)
        let cost = calculate_cost_micros(p_sonnet, 100_000, 5_000, 80_000, 0);
        assert_eq!(cost, 159_000);
    }

    #[test]
    fn test_2026_flagship_model_lookups_and_aliases() {
        let p_sonnet55 = find_model_price("claude-sonnet-5.5").expect("claude-sonnet-5.5");
        assert_eq!(p_sonnet55.input_micros_per_m, 2_000_000);
        assert_eq!(p_sonnet55.output_micros_per_m, 10_000_000);

        let p_astra = find_model_price("gpt-6.2-astra").expect("gpt-6.2-astra");
        assert_eq!(p_astra.input_micros_per_m, 8_000_000);
        assert_eq!(p_astra.output_micros_per_m, 40_000_000);

        let p_neon = find_model_price("gemini-4-neon").expect("gemini-4-neon");
        assert_eq!(p_neon.input_micros_per_m, 3_000_000);

        let p_v4 = find_model_price("deepseek-v4.1-flash").expect("deepseek-v4.1-flash");
        assert_eq!(p_v4.input_micros_per_m, 36_000);

        // Aliases
        assert_eq!(
            find_model_price("sol").unwrap().input_micros_per_m,
            2_000_000
        );
        assert_eq!(
            find_model_price("luna").unwrap().input_micros_per_m,
            100_000
        );
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0), "$0.0000");
        assert_eq!(format_cost(25), "< $0.0001");
        assert_eq!(format_cost(8_400), "$0.0084");
        assert_eq!(format_cost(1_245_000), "$1.2450");
        assert_eq!(format_cost(18_000_000), "$18.0000");
    }

    #[test]
    fn test_estimate_cost_with_db_precedence() {
        use crate::store::db::{Database, DynamicPriceRow};

        let db = Database::open_memory().unwrap();

        // 1. Built-in catalog: claude-sonnet-5.5 ($2.00 / 1M in, $10.00 / 1M out)
        // 1M prompt + 1M completion = $12.00 = 12_000_000 micros
        let cost_builtin = estimate_cost_with_db(
            Some(&db),
            Some("claude-sonnet-5.5"),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost_builtin, 12_000_000);

        // 2. Synced pricing overrides built-in: $1.80 in, $9.00 out -> $10.80 = 10_800_000 micros
        let row_synced = DynamicPriceRow {
            model_pattern: "claude-sonnet-5.5".to_string(),
            input_micros_per_m: 1_800_000,
            output_micros_per_m: 9_000_000,
            cache_read_micros_per_m: 90_000,
            cache_write_micros_per_m: 2_250_000,
            source: "synced".to_string(),
            updated_at: "2026-11-20T20:00:00Z".to_string(),
        };
        db.upsert_model_pricing(&row_synced).unwrap();

        let cost_synced = estimate_cost_with_db(
            Some(&db),
            Some("claude-sonnet-5.5"),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost_synced, 10_800_000);

        // 3. Custom pricing overrides synced: $1.00 in, $5.00 out -> $6.00 = 6_000_000 micros
        let row_custom = DynamicPriceRow {
            model_pattern: "claude-sonnet-5.5".to_string(),
            input_micros_per_m: 1_000_000,
            output_micros_per_m: 5_000_000,
            cache_read_micros_per_m: 50_000,
            cache_write_micros_per_m: 1_250_000,
            source: "custom".to_string(),
            updated_at: "2026-11-20T20:05:00Z".to_string(),
        };
        db.upsert_model_pricing(&row_custom).unwrap();

        let cost_custom = estimate_cost_with_db(
            Some(&db),
            Some("claude-sonnet-5.5"),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost_custom, 6_000_000);

        // 4. Unknown model returns 0
        let cost_unknown = estimate_cost_with_db(
            Some(&db),
            Some("my-unregistered-model"),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost_unknown, 0);

        // 5. Delete custom -> falls back to synced
        db.delete_model_pricing("claude-sonnet-5.5").unwrap();
        let cost_fallback_synced = estimate_cost_with_db(
            Some(&db),
            Some("claude-sonnet-5.5"),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost_fallback_synced, 10_800_000);

        // 6. Clear synced -> falls back to built-in
        db.clear_synced_model_pricing().unwrap();
        let cost_fallback_builtin = estimate_cost_with_db(
            Some(&db),
            Some("claude-sonnet-5.5"),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost_fallback_builtin, 12_000_000);
    }
}
