use serde_json::Value;

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

impl Usage {
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
}
