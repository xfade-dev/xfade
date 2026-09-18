use serde_json::Value;

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

impl Usage {
    /// Extract usage from a non-stream JSON response body. Compatible with
    /// OpenAI (`prompt_tokens`/`completion_tokens`) and Anthropic
    /// (`input_tokens`/`output_tokens`) shapes.
    pub fn from_json(bytes: &[u8]) -> Self {
        let v: Value = match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        let usage = v.get("usage").cloned().unwrap_or(Value::Null);
        let prompt = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        let completion = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
        }
    }

    /// Extract usage from the tail of an SSE stream.
    ///
    /// Parses each `data: <json>` line (the `[DONE]` sentinel is skipped) and
    /// merges usage information:
    ///   - `prompt_tokens` or `input_tokens` (in a usage block) updates the
    ///     prompt count;
    ///   - `completion_tokens` or `output_tokens` (in a usage block) updates
    ///     the completion count.
    ///
    /// Note: in Anthropic's streaming protocol, `message_delta` events carry a
    /// cumulative `output_tokens` (the running total for the response), so we
    /// take the last seen value — which `take_last` semantics provide by
    /// always overwriting when a value is present.
    ///
    /// The rules above cover OpenAI (`chat`/`responses`) and Anthropic
    /// (`messages`) streams.
    pub fn from_sse_tail(tail: &[u8]) -> Self {
        let text = String::from_utf8_lossy(tail);
        let mut prompt: Option<i64> = None;
        let mut completion: Option<i64> = None;

        for line in text.lines() {
            let trimmed = line.trim_start();
            let payload = if let Some(rest) = trimmed.strip_prefix("data:") {
                rest.trim_start()
            } else {
                continue;
            };
            if payload == "[DONE]" {
                continue;
            }
            let v: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Usage may live at the top level of the event (OpenAI final
            // chunk with include_usage), inside `usage`, or — for Anthropic's
            // `message_start` — nested under `message.usage`. `message_delta`
            // carries `usage` directly.
            let usage = v
                .get("usage")
                .cloned()
                .or_else(|| v.get("message").and_then(|m| m.get("usage")).cloned())
                .or_else(|| v.get("response").and_then(|m| m.get("usage")).cloned())
                .unwrap_or(Value::Null);
            if usage.is_null() {
                continue;
            }
            if let Some(p) = usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(|t| t.as_i64())
            {
                prompt = Some(p);
            }
            if let Some(c) = usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(|t| t.as_i64())
            {
                // message_delta's output_tokens is cumulative → take last.
                completion = Some(c);
            }
        }

        Self {
            prompt_tokens: prompt.unwrap_or(0),
            completion_tokens: completion.unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_sse_tail_openai_with_usage_chunk() {
        let tail = b"data: {\"choices\":[{\"delta\":{\"content\":\"o\"}}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":1}}\n\n\
data: [DONE]\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 7);
        assert_eq!(u.completion_tokens, 1);
    }

    #[test]
    fn from_sse_tail_anthropic_cumulative_output() {
        let tail = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 12);
        // last seen cumulative output_tokens wins
        assert_eq!(u.completion_tokens, 5);
    }

    #[test]
    fn from_sse_tail_no_usage_returns_zero() {
        let tail = b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        let u = Usage::from_sse_tail(tail);
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
    }
}
