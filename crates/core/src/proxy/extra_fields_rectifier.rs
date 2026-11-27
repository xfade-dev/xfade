use serde_json::Value;

/// Core Anthropic content-block keys that must never be stripped even in 400 fallback mode.
const PROTECTED_BLOCK_KEYS: &[&str] = &[
    "type",
    "id",
    "name",
    "input",
    "tool_use_id",
    "content",
    "is_error",
    "cache_control",
];

/// Strip `null`-valued keys from `messages[*].content[*]` `tool_use` and `tool_result`
/// blocks (e.g. `toolset_name: null`, `caller: null`, or any future `foo: null`).
///
/// Non-null fields, nested `input` payloads, and top-level request fields are untouched.
/// Returns the number of keys removed.
pub fn strip_null_extras(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return 0;
    };

    let mut removed = 0usize;
    for msg in messages {
        let Some(content) = msg.get_mut("content").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for block in content {
            let Some(obj) = block.as_object_mut() else {
                continue;
            };
            let is_tool_block = matches!(
                obj.get("type").and_then(|v| v.as_str()),
                Some("tool_use" | "tool_result")
            );
            if !is_tool_block {
                continue;
            }
            let keys_to_remove: Vec<String> = obj
                .iter()
                .filter(|(k, v)| v.is_null() && !PROTECTED_BLOCK_KEYS.contains(&k.as_str()))
                .map(|(k, _)| k.clone())
                .collect();
            for k in keys_to_remove {
                if obj.remove(&k).is_some() {
                    removed += 1;
                }
            }
        }
    }
    removed
}

/// Inspects an upstream 400 Bad Request response for extra-field validation errors
/// (e.g. LiteLLM/Bedrock `"Extra inputs are not permitted"`) and strips offending
/// fields from `tool_use` / `tool_result` blocks so the request can be retried once.
pub fn rectify_extra_fields_error(body: &mut Value, error_status: u16, error_body: &str) -> bool {
    if error_status != 400 || !body.is_object() {
        return false;
    }

    let error_lower = error_body.to_ascii_lowercase();
    if !indicates_extra_fields_error(&error_lower) {
        return false;
    }

    if strip_null_extras(body) > 0 {
        return true;
    }

    // Fallback: if no null fields remain, strip non-protected keys on tool_use / tool_result
    // blocks that are explicitly named in the upstream error message.
    strip_named_tool_extras(body, &error_lower) > 0
}

fn indicates_extra_fields_error(error_lower: &str) -> bool {
    error_lower.contains("extra inputs are not permitted")
        || error_lower.contains("extra fields not permitted")
        || error_lower.contains("additional properties are not allowed")
}

fn strip_named_tool_extras(body: &mut Value, error_lower: &str) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) else {
        return 0;
    };

    let mut removed = 0usize;
    for msg in messages {
        let Some(content) = msg.get_mut("content").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for block in content {
            let Some(obj) = block.as_object_mut() else {
                continue;
            };
            let is_tool_block = matches!(
                obj.get("type").and_then(|v| v.as_str()),
                Some("tool_use" | "tool_result")
            );
            if !is_tool_block {
                continue;
            }
            let keys_to_remove: Vec<String> = obj
                .keys()
                .filter(|k| {
                    !PROTECTED_BLOCK_KEYS.contains(&k.as_str())
                        && error_lower.contains(&k.to_ascii_lowercase())
                })
                .cloned()
                .collect();
            for k in keys_to_remove {
                if obj.remove(&k).is_some() {
                    removed += 1;
                }
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_null_extras_removes_null_tool_fields_and_keeps_non_null() {
        let mut body = json!({
            "model": "claude-sonnet-4-6",
            "top_null": null,
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "bash",
                            "input": { "cmd": "ls", "opt": null },
                            "toolset_name": null,
                            "caller": null,
                            "future_null": null,
                            "non_null_caller": "computer"
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_1",
                            "content": "ok",
                            "extra_result_flag": null
                        }
                    ]
                }
            ]
        });

        let count = strip_null_extras(&mut body);
        assert_eq!(count, 4);

        let tool_use = &body["messages"][0]["content"][0];
        assert!(tool_use.get("toolset_name").is_none());
        assert!(tool_use.get("caller").is_none());
        assert!(tool_use.get("future_null").is_none());
        assert_eq!(tool_use["non_null_caller"], "computer");
        assert_eq!(tool_use["id"], "toolu_1");
        assert_eq!(tool_use["name"], "bash");
        assert_eq!(tool_use["input"]["opt"], json!(null));
        assert_eq!(body["top_null"], json!(null));

        let tool_result = &body["messages"][1]["content"][0];
        assert!(tool_result.get("extra_result_flag").is_none());
        assert_eq!(tool_result["tool_use_id"], "toolu_1");
    }

    #[test]
    fn strip_null_extras_returns_zero_when_clean() {
        let mut body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "bash",
                            "input": { "cmd": "ls" }
                        }
                    ]
                }
            ]
        });
        let snapshot = body.clone();
        assert_eq!(strip_null_extras(&mut body), 0);
        assert_eq!(body, snapshot);
    }

    #[test]
    fn rectify_extra_fields_error_only_triggers_on_matching_400() {
        let orig = json!({
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "bash",
                    "input": {},
                    "toolset_name": null,
                    "custom_flag": "v1"
                }]
            }]
        });

        // Ignores non-400 statuses (403, 422, 500)
        for status in [403, 422, 500] {
            let mut b = orig.clone();
            assert!(!rectify_extra_fields_error(
                &mut b,
                status,
                "Extra inputs are not permitted"
            ));
        }

        // Ignores unrelated 400
        let mut b = orig.clone();
        assert!(!rectify_extra_fields_error(&mut b, 400, "invalid api key"));

        // Strips null fields first on matching 400
        let mut b = orig.clone();
        assert!(rectify_extra_fields_error(
            &mut b,
            400,
            "litellm.BadRequestError: #/messages/0/content/0/tool_use.toolset_name: Extra inputs are not permitted"
        ));
        assert!(b["messages"][0]["content"][0].get("toolset_name").is_none());
        assert_eq!(b["messages"][0]["content"][0]["custom_flag"], "v1");

        // Fallback: if no null fields left, strips named extra key on matching 400
        assert!(rectify_extra_fields_error(
            &mut b,
            400,
            "Additional properties are not allowed ('custom_flag' was unexpected)"
        ));
        assert!(b["messages"][0]["content"][0].get("custom_flag").is_none());
    }
}
