//! Token pricing catalog and cost estimation for supported models.

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
    // Anthropic models
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
    // OpenAI models
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
    // DeepSeek models
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
    // Google Gemini models
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
];

/// Find the model price entry using prefix and alias normalization.
pub fn find_model_price(model_name: &str) -> Option<&'static ModelPrice> {
    let trimmed = model_name.trim().to_lowercase();
    // Strip multi-tier provider prefixes if present (e.g. "openrouter/anthropic/claude-3-7-sonnet" -> "claude-3-7-sonnet")
    let raw_name = if let Some((_, rest)) = trimmed.rsplit_once('/') {
        rest
    } else {
        &trimmed
    };

    // Normalize common model aliases
    let canonical_name: &str = match raw_name {
        "sonnet" => "claude-3-7-sonnet",
        "opus" => "claude-3-opus",
        "haiku" => "claude-3-5-haiku",
        "4o" => "gpt-4o",
        "4o-mini" => "gpt-4o-mini",
        other => other,
    };

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

/// Convenience helper to estimate cost for a named model, defaulting to 0 if unknown.
pub fn estimate_cost(
    model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_prompt_tokens: i64,
    cache_write_tokens: i64,
) -> i64 {
    let Some(m) = model else { return 0 };
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
        assert_eq!(p_alias.input_micros_per_m, 3_000_000);
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
    fn test_format_cost() {
        assert_eq!(format_cost(0), "$0.0000");
        assert_eq!(format_cost(25), "< $0.0001");
        assert_eq!(format_cost(8_400), "$0.0084");
        assert_eq!(format_cost(1_245_000), "$1.2450");
        assert_eq!(format_cost(18_000_000), "$18.0000");
    }
}
