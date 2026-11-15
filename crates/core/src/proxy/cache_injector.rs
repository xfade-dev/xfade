use serde_json::{json, Value};

/// Report generated after attempting to inject Anthropic `cache_control` breakpoints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CacheInjectionReport {
    /// Number of `cache_control` breakpoints already present in the request body.
    pub existing_breakpoints: usize,
    /// Number of new `cache_control` breakpoints injected.
    pub injected_breakpoints: usize,
    /// Locations where new breakpoints were injected.
    /// Possible values: `"tools"`, `"system"`, `"messages_latest"`, `"messages_penultimate"`.
    pub locations: Vec<&'static str>,
}

/// Checks whether a JSON value has a non-null `cache_control` field.
fn has_cache_control(val: &Value) -> bool {
    val.get("cache_control").is_some_and(|cc| !cc.is_null())
}

/// Counts existing `cache_control` breakpoints in `tools`, `system`, and `messages`.
fn count_existing_breakpoints(body: &Value) -> usize {
    let mut count = 0;

    // 1. tools
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            if has_cache_control(tool) {
                count += 1;
            }
        }
    }

    // 2. system
    if let Some(system) = body.get("system") {
        match system {
            Value::Array(blocks) => {
                for block in blocks {
                    if has_cache_control(block) {
                        count += 1;
                    }
                }
            }
            Value::Object(_) if has_cache_control(system) => {
                count += 1;
            }
            _ => {}
        }
    }

    // 3. messages
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for msg in messages {
            if has_cache_control(msg) {
                count += 1;
            }
            if let Some(content) = msg.get("content") {
                match content {
                    Value::Array(blocks) => {
                        for block in blocks {
                            if has_cache_control(block) {
                                count += 1;
                            }
                        }
                    }
                    Value::Object(_) if has_cache_control(content) => {
                        count += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    count
}

/// Checks if a message already has a `cache_control` breakpoint anywhere on it.
fn message_has_cache_control(msg: &Value) -> bool {
    if has_cache_control(msg) {
        return true;
    }
    if let Some(content) = msg.get("content") {
        match content {
            Value::Array(blocks) => blocks.iter().any(has_cache_control),
            Value::Object(_) => has_cache_control(content),
            _ => false,
        }
    } else {
        false
    }
}

/// Checks whether a message contains ONLY thinking blocks (e.g. Claude 3.7 extended thinking).
fn is_thinking_only_message(msg: &Value) -> bool {
    if msg.get("role").and_then(Value::as_str) == Some("thinking") {
        return true;
    }
    if let Some(content) = msg.get("content").and_then(Value::as_array) {
        if content.is_empty() {
            return false;
        }
        content.iter().all(|block| {
            let t = block.get("type").and_then(Value::as_str);
            t == Some("thinking") || t == Some("redacted_thinking")
        })
    } else {
        false
    }
}

/// Injects `cache_control: {"type": "ephemeral"}` into a message.
///
/// If `content` is a string, it is converted to a block array:
/// `[{"type": "text", "text": s, "cache_control": {"type": "ephemeral"}}]`.
/// If `content` is an array of blocks, the last eligible text or tool_result block
/// (or other non-thinking block) receives the `cache_control`.
fn inject_into_message(msg: &mut Value) -> bool {
    if message_has_cache_control(msg) {
        return false;
    }
    match msg.get_mut("content") {
        Some(Value::String(s)) => {
            let text = s.clone();
            msg["content"] = json!([
                {
                    "type": "text",
                    "text": text,
                    "cache_control": {
                        "type": "ephemeral"
                    }
                }
            ]);
            true
        }
        Some(Value::Array(blocks)) if !blocks.is_empty() => {
            // Priority 1: last text or tool_result block
            let eligible_idx = blocks
                .iter()
                .rposition(|b| {
                    if let Some(t) = b.get("type").and_then(Value::as_str) {
                        t == "text" || t == "tool_result"
                    } else {
                        b.get("text").is_some()
                    }
                })
                .or_else(|| {
                    // Priority 2: fallback to any non-thinking block
                    blocks.iter().rposition(|b| {
                        if let Some(t) = b.get("type").and_then(Value::as_str) {
                            t != "thinking" && t != "redacted_thinking"
                        } else {
                            true
                        }
                    })
                });

            if let Some(idx) = eligible_idx {
                let block = &mut blocks[idx];
                if block.get("cache_control").is_none() {
                    block["cache_control"] = json!({
                        "type": "ephemeral"
                    });
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Automatically injects Anthropic prompt cache (`cache_control: {"type": "ephemeral"}`)
/// breakpoints into the given JSON request body, respecting a maximum breakpoint budget.
///
/// Breakpoints are injected in optimal sequence:
/// 1. `tools` last item.
/// 2. `system` last item (converts scalar string to array of blocks if needed).
/// 3. `messages` latest non-thinking message (last text or tool_result block).
/// 4. Penultimate user message if budget remains.
pub fn inject_cache_control(body: &mut Value, max_breakpoints: usize) -> CacheInjectionReport {
    let existing_breakpoints = count_existing_breakpoints(body);
    let mut remaining_budget = max_breakpoints.saturating_sub(existing_breakpoints);
    let mut injected_breakpoints = 0;
    let mut locations = Vec::new();

    if remaining_budget == 0 {
        return CacheInjectionReport {
            existing_breakpoints,
            injected_breakpoints: 0,
            locations,
        };
    }

    // Step 1: tools last item
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        if let Some(last_tool) = tools.last_mut() {
            if last_tool.is_object() && !has_cache_control(last_tool) {
                last_tool["cache_control"] = json!({"type": "ephemeral"});
                remaining_budget -= 1;
                injected_breakpoints += 1;
                locations.push("tools");
            }
        }
    }

    // Step 2: system last item
    if remaining_budget > 0 {
        if let Some(system_val) = body.get_mut("system") {
            match system_val {
                Value::String(s) if !s.is_empty() => {
                    let text = s.clone();
                    *system_val = json!([
                        {
                            "type": "text",
                            "text": text,
                            "cache_control": {
                                "type": "ephemeral"
                            }
                        }
                    ]);
                    remaining_budget -= 1;
                    injected_breakpoints += 1;
                    locations.push("system");
                }
                Value::Array(blocks) if !blocks.is_empty() => {
                    if let Some(last_block) = blocks.last_mut() {
                        if last_block.is_object() && !has_cache_control(last_block) {
                            last_block["cache_control"] = json!({"type": "ephemeral"});
                            remaining_budget -= 1;
                            injected_breakpoints += 1;
                            locations.push("system");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Steps 3 & 4: messages latest non-thinking message & penultimate user message
    if remaining_budget > 0 {
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            let mut latest_msg_idx = None;

            // Step 3: Find latest non-thinking message
            for i in (0..messages.len()).rev() {
                let msg = &mut messages[i];
                if is_thinking_only_message(msg) {
                    continue;
                }
                if inject_into_message(msg) {
                    remaining_budget -= 1;
                    injected_breakpoints += 1;
                    locations.push("messages_latest");
                    latest_msg_idx = Some(i);
                    break;
                } else if message_has_cache_control(msg) {
                    // Latest non-thinking message already has cache control
                    latest_msg_idx = Some(i);
                    break;
                }
            }

            // Step 4: Penultimate user message
            if remaining_budget > 0 {
                let user_indices: Vec<usize> = messages
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, m)| {
                        if m.get("role").and_then(Value::as_str) == Some("user") {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .collect();

                if user_indices.len() >= 2 {
                    let penultimate_idx = user_indices[user_indices.len() - 2];
                    if Some(penultimate_idx) != latest_msg_idx {
                        let penult_msg = &mut messages[penultimate_idx];
                        if inject_into_message(penult_msg) {
                            #[allow(unused_assignments)]
                            {
                                remaining_budget -= 1;
                            }
                            injected_breakpoints += 1;
                            locations.push("messages_penultimate");
                        }
                    }
                }
            }
        }
    }

    CacheInjectionReport {
        existing_breakpoints,
        injected_breakpoints,
        locations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_inject_cache_control_tools_and_system() {
        let mut body = json!({
            "system": "You are a helpful coding assistant.",
            "tools": [{"name": "bash", "description": "run command"}],
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });
        let report = inject_cache_control(&mut body, 4);
        assert_eq!(report.existing_breakpoints, 0);
        assert_eq!(report.injected_breakpoints, 3);
        assert!(report.locations.contains(&"tools"));
        assert!(report.locations.contains(&"system"));
        assert!(report.locations.contains(&"messages_latest"));

        // system must be converted to array with ephemeral cache_control
        assert!(body["system"].is_array());
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_inject_cache_control_respects_budget_limit_of_4() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "rule 1", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "rule 2", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "rule 3", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}}]},
                {"role": "assistant", "content": "hi"},
                {"role": "user", "content": "next task"}
            ]
        });
        let report = inject_cache_control(&mut body, 4);
        assert_eq!(report.existing_breakpoints, 4);
        assert_eq!(report.injected_breakpoints, 0);
        assert!(report.locations.is_empty());
    }

    #[test]
    fn test_inject_all_four_breakpoints_with_multi_turn() {
        let mut body = json!({
            "tools": [
                {"name": "grep", "description": "search file"},
                {"name": "edit", "description": "edit file"}
            ],
            "system": "System instructions here",
            "messages": [
                {"role": "user", "content": "first message from user"},
                {"role": "assistant", "content": "assistant response 1"},
                {"role": "user", "content": "second message from user"}
            ]
        });

        let report = inject_cache_control(&mut body, 4);
        assert_eq!(report.existing_breakpoints, 0);
        assert_eq!(report.injected_breakpoints, 4);
        assert_eq!(
            report.locations,
            vec!["tools", "system", "messages_latest", "messages_penultimate"]
        );

        // Verify tools: only the last tool has cache_control
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");

        // Verify system: converted to block array with cache_control
        assert_eq!(body["system"][0]["text"], "System instructions here");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");

        // Verify messages_latest: user message 2 (messages[2])
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );

        // Verify messages_penultimate: user message 1 (messages[0])
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );

        // Assistant message should not have cache_control
        assert!(body["messages"][1].get("cache_control").is_none());
    }

    #[test]
    fn test_inject_skips_thinking_only_messages() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "Can you solve this?"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "Thinking deeply..."}
                    ]
                }
            ]
        });

        let report = inject_cache_control(&mut body, 4);
        // messages[1] is thinking-only, so latest non-thinking message is messages[0]
        assert_eq!(report.injected_breakpoints, 1);
        assert_eq!(report.locations, vec!["messages_latest"]);
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        // Thinking block must NOT have cache_control
        assert!(body["messages"][1]["content"][0]
            .get("cache_control")
            .is_none());
    }

    #[test]
    fn test_inject_with_thinking_and_text_assistant_block() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "Question 1"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "Let's think..."},
                        {"type": "text", "text": "Answer 1"}
                    ]
                }
            ]
        });

        let report = inject_cache_control(&mut body, 4);
        // messages[1] contains text, so it is eligible as latest non-thinking message
        assert_eq!(report.injected_breakpoints, 1);
        assert_eq!(report.locations, vec!["messages_latest"]);
        // Thinking block untouched
        assert!(body["messages"][1]["content"][0]
            .get("cache_control")
            .is_none());
        // Text block receives cache_control
        assert_eq!(
            body["messages"][1]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn test_inject_with_tool_result_content() {
        let mut body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "call_1", "content": "File lines: 100"}
                    ]
                }
            ]
        });

        let report = inject_cache_control(&mut body, 4);
        assert_eq!(report.injected_breakpoints, 1);
        assert_eq!(report.locations, vec!["messages_latest"]);
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn test_inject_system_as_existing_blocks_array() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "Base instructions"},
                {"type": "text", "text": "Extra prompt details"}
            ]
        });

        let report = inject_cache_control(&mut body, 4);
        assert_eq!(report.injected_breakpoints, 1);
        assert_eq!(report.locations, vec!["system"]);

        // First block has no cache_control
        assert!(body["system"][0].get("cache_control").is_none());
        // Last block gets cache_control
        assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_inject_respects_partial_budget() {
        let mut body = json!({
            "tools": [{"name": "tool1", "description": "desc"}],
            "system": "System prompt",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        // Only 1 breakpoint allowed
        let report = inject_cache_control(&mut body, 1);
        assert_eq!(report.existing_breakpoints, 0);
        assert_eq!(report.injected_breakpoints, 1);
        assert_eq!(report.locations, vec!["tools"]);
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["system"].is_string()); // system was not converted or touched
    }

    #[test]
    fn test_inject_empty_body() {
        let mut body = json!({});
        let report = inject_cache_control(&mut body, 4);
        assert_eq!(report.existing_breakpoints, 0);
        assert_eq!(report.injected_breakpoints, 0);
        assert!(report.locations.is_empty());
    }

    #[test]
    fn test_inject_zero_budget() {
        let mut body = json!({
            "system": "Hello"
        });
        let report = inject_cache_control(&mut body, 0);
        assert_eq!(report.existing_breakpoints, 0);
        assert_eq!(report.injected_breakpoints, 0);
        assert!(report.locations.is_empty());
    }
}
