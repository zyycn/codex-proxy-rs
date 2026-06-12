use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codex::transport::sse::{parse_sse_events, SseError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn new(input_tokens: u64, output_tokens: u64, cached_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_tokens,
            total_tokens: input_tokens + output_tokens,
        }
    }

    pub fn merged(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cached_tokens: self.cached_tokens + other.cached_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
        }
    }
}

pub fn extract_usage(body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usage").unwrap_or(body);
    if !usage.is_object() {
        return None;
    }

    let input_tokens = number_field(usage, "input_tokens")
        .or_else(|| number_field(usage, "prompt_tokens"))
        .unwrap_or_default();
    let output_tokens = number_field(usage, "output_tokens")
        .or_else(|| number_field(usage, "completion_tokens"))
        .unwrap_or_default();
    let cached_tokens = nested_number_field(usage, &["input_tokens_details", "cached_tokens"])
        .or_else(|| nested_number_field(usage, &["prompt_tokens_details", "cached_tokens"]))
        .or_else(|| number_field(usage, "cached_tokens"))
        .unwrap_or_default();
    let total_tokens = number_field(usage, "total_tokens").unwrap_or(input_tokens + output_tokens);

    let has_usage = [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "total_tokens",
    ]
    .iter()
    .any(|field| usage.get(*field).is_some())
        || usage.get("input_tokens_details").is_some()
        || usage.get("prompt_tokens_details").is_some();
    has_usage.then_some(TokenUsage {
        input_tokens,
        output_tokens,
        cached_tokens,
        total_tokens,
    })
}

pub fn extract_sse_usage(body: &str) -> Result<Option<TokenUsage>, SseError> {
    let events = parse_sse_events(body)?;
    let mut usage: Option<TokenUsage> = None;
    for event in events {
        if event.data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let event_usage =
            extract_usage(&value).or_else(|| value.get("response").and_then(extract_usage));
        if let Some(event_usage) = event_usage {
            usage = Some(match usage {
                Some(current) => current.merged(event_usage),
                None => event_usage,
            });
        }
    }
    Ok(usage)
}

fn number_field(value: &Value, field: &str) -> Option<u64> {
    value.get(field)?.as_u64()
}

fn nested_number_field(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_u64()
}
