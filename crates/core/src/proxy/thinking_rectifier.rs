use serde_json::Value;

/// Maximum cap for `max_tokens` when adjusting for thinking budget.
pub const MAX_TOKENS_CAP: u64 = 64_000;

/// Default buffer added above `budget_tokens` to provide tokens for the model response.
pub const BUDGET_BUFFER_TOKENS: u64 = 1_024;

/// Inspects an upstream error response (specifically 400 Bad Request) and attempts to self-heal
/// thinking-related parameters in `body`.
///
/// Returns `true` if the request payload was modified and can be retried immediately;
/// returns `false` if the error could not be rectified.
pub fn rectify_thinking_error(body: &mut Value, error_status: u16, error_body: &str) -> bool {
    // Only 400 Bad Request errors are considered for request parameter rectification.
    if error_status != 400 || !body.is_object() {
        return false;
    }

    let error_lower = error_body.to_ascii_lowercase();

    // Case 1: max_tokens <= budget_tokens error.
    // Upstream (e.g. Anthropic Claude 3.7) requires `max_tokens > budget_tokens`.
    if error_lower.contains("budget_tokens") && error_lower.contains("max_tokens") {
        if let Some(budget_tokens) = extract_budget_tokens(&error_lower, body) {
            let new_max = budget_tokens
                .saturating_add(BUDGET_BUFFER_TOKENS)
                .min(MAX_TOKENS_CAP);
            body["max_tokens"] = serde_json::json!(new_max);
            return true;
        }
    }

    // Case 2: Upstream model or provider does not support thinking parameters.
    // e.g. "thinking is not supported for this model", "unsupported parameter: thinking".
    if indicates_thinking_not_supported(&error_lower) {
        if let Some(obj) = body.as_object_mut() {
            if obj.remove("thinking").is_some() {
                return true;
            }
        }
    }

    false
}

/// Attempts to extract the budget tokens amount, first from the error body text,
/// and falling back to `body["thinking"]["budget_tokens"]`.
fn extract_budget_tokens(error_lower: &str, body: &Value) -> Option<u64> {
    if let Some(val) = extract_budget_from_error(error_lower) {
        return Some(val);
    }
    extract_budget_from_body(body)
}

/// Scans for consecutive digits immediately following "budget_tokens" in the error text.
fn extract_budget_from_error(error_lower: &str) -> Option<u64> {
    let idx = error_lower.find("budget_tokens")?;
    let after = &error_lower[idx + "budget_tokens".len()..];

    let mut chars = after.chars();
    let mut skipped = 0;
    while let Some(c) = chars.next() {
        skipped += 1;
        if skipped > 40 {
            break;
        }
        if c.is_ascii_digit() {
            let mut digits = String::new();
            digits.push(c);
            for d in chars.by_ref() {
                if d.is_ascii_digit() {
                    digits.push(d);
                } else {
                    break;
                }
            }
            if let Ok(num) = digits.parse::<u64>() {
                if num > 0 {
                    return Some(num);
                }
            }
            break;
        } else if !matches!(
            c,
            ':' | '=' | ' ' | '\t' | '"' | '\'' | '(' | ')' | '`' | '\\'
        ) {
            // Hit a non-delimiter character before finding digits
            break;
        }
    }
    None
}

/// Reads the budget tokens integer or string value from the request payload.
fn extract_budget_from_body(body: &Value) -> Option<u64> {
    let thinking = body.get("thinking")?;
    let budget = thinking.get("budget_tokens")?;
    if let Some(n) = budget.as_u64() {
        if n > 0 {
            return Some(n);
        }
    } else if let Some(s) = budget.as_str() {
        if let Ok(n) = s.parse::<u64>() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    None
}

/// Checks whether the error text indicates that the upstream rejected the thinking parameter.
fn indicates_thinking_not_supported(error_lower: &str) -> bool {
    if !error_lower.contains("thinking") {
        return false;
    }

    error_lower.contains("thinking is not supported")
        || error_lower.contains("not supported")
        || error_lower.contains("unsupported")
        || error_lower.contains("not support")
        || error_lower.contains("not allowed")
        || error_lower.contains("not enabled")
        || error_lower.contains("unrecognized")
        || error_lower.contains("unknown")
        || error_lower.contains("unexpected")
        || error_lower.contains("extra fields not permitted")
        || error_lower.contains("extra inputs are not permitted")
        || error_lower.contains("additional properties are not allowed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_rectify_max_tokens_less_than_budget() {
        let mut body = json!({
            "model": "claude-3-7-sonnet-20250219",
            "max_tokens": 2048,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 4096
            }
        });
        let error_msg = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 2048 must be greater than budget_tokens: 4096"}}"#;
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(rectified);
        assert_eq!(body["max_tokens"], 4096 + 1024);
    }

    #[test]
    fn test_rectify_thinking_not_supported_strips_block() {
        let mut body = json!({
            "model": "deepseek-chat",
            "max_tokens": 4096,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 2048
            }
        });
        let error_msg = r#"{"error":{"message":"thinking is not supported for this model","type":"invalid_request_error"}}"#;
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(rectified);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn test_rectify_max_tokens_capped_at_64000() {
        let mut body = json!({
            "model": "claude-3-7-sonnet-20250219",
            "max_tokens": 1000,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 63500
            }
        });
        let error_msg = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 1000 must be greater than budget_tokens: 63500"}}"#;
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(rectified);
        assert_eq!(body["max_tokens"], 64000);
    }

    #[test]
    fn test_rectify_ignores_non_400_status() {
        let mut body = json!({
            "model": "claude-3-7-sonnet-20250219",
            "max_tokens": 2048,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 4096
            }
        });
        let error_msg = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 2048 must be greater than budget_tokens: 4096"}}"#;
        // 500 or 403 should not be rectified by thinking_rectifier
        assert!(!rectify_thinking_error(&mut body, 500, error_msg));
        assert_eq!(body["max_tokens"], 2048);
        assert!(!rectify_thinking_error(&mut body, 403, error_msg));
        assert_eq!(body["max_tokens"], 2048);
    }

    #[test]
    fn test_rectify_ignores_unrelated_400_errors() {
        let mut body = json!({
            "model": "gpt-4o",
            "messages": []
        });
        let error_msg = r#"{"error":{"message":"Invalid model name specified","type":"invalid_request_error"}}"#;
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(!rectified);
    }

    #[test]
    fn test_rectify_non_object_body() {
        let mut body = json!("just a string");
        let error_msg = r#"{"message":"thinking is not supported"}"#;
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(!rectified);
    }

    #[test]
    fn test_rectify_extracts_budget_from_body_fallback() {
        let mut body = json!({
            "model": "claude-3-7-sonnet-20250219",
            "max_tokens": 1000,
            "thinking": {
                "type": "enabled",
                "budget_tokens": 3000
            }
        });
        // Error message does not have the number after budget_tokens
        let error_msg = r#"{"error":{"message":"max_tokens must be greater than budget_tokens"}}"#;
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(rectified);
        assert_eq!(body["max_tokens"], 3000 + 1024);
    }

    #[test]
    fn test_rectify_thinking_not_supported_no_thinking_field() {
        let mut body = json!({
            "model": "deepseek-chat",
            "max_tokens": 4096
        });
        let error_msg = r#"{"error":{"message":"thinking is not supported for this model"}}"#;
        // Body has no thinking key to remove, so nothing to rectify
        let rectified = rectify_thinking_error(&mut body, 400, error_msg);
        assert!(!rectified);
    }

    #[test]
    fn test_rectify_various_unsupported_error_messages() {
        let cases = [
            r#"{"message":"Unsupported parameter: thinking"}"#,
            r#"{"message":"model does not support thinking mode"}"#,
            r#"{"message":"thinking is not enabled for this account"}"#,
            r#"{"message":"extra fields not permitted: thinking"}"#,
        ];

        for error_msg in cases {
            let mut body = json!({
                "model": "custom-model",
                "thinking": { "type": "enabled" }
            });
            let rectified = rectify_thinking_error(&mut body, 400, error_msg);
            assert!(rectified, "Failed on: {}", error_msg);
            assert!(body.get("thinking").is_none());
        }
    }
}
