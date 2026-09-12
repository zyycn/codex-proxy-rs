//! 诊断内容的统一筛选边界。未知键名与值只保留有界结构和摘要。

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// 原始字节的长度与 SHA-256，不复制或保留正文。
#[must_use]
pub fn body_fingerprint(bytes: &[u8]) -> Value {
    json!({"bytes": bytes.len(), "sha256": fingerprint_hex(bytes)})
}

fn fingerprint_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .flat_map(|byte| {
            let alphabet = b"0123456789abcdef";
            [
                char::from(alphabet[usize::from(byte >> 4)]),
                char::from(alphabet[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

/// 仅保留有限协议骨架；任意 JSON（包括 headers 和 metadata）不能声明自己可信。
#[must_use]
pub fn diagnostic_json(value: &Value) -> Value {
    capture(value, "", Context::Protocol, 0, &mut 96)
}

/// 只有 transport 的上游 metadata / error 帧顶层才是受控头部边界。
pub(super) fn diagnostic_event_json(value: &Value, stage: &str) -> Value {
    let context = if stage == "upstream.event"
        && value.get("headers").is_some_and(Value::is_object)
        && matches!(
            value.get("type").and_then(Value::as_str),
            Some("response.metadata" | "codex.response.metadata" | "error")
        ) {
        Context::UpstreamEvent
    } else {
        Context::Protocol
    };
    capture(value, "", context, 0, &mut 96)
}

#[derive(Clone, Copy)]
enum Context {
    Protocol,
    UpstreamEvent,
    Opaque,
    Headers,
    HeaderValue,
}

fn capture(value: &Value, key: &str, context: Context, depth: usize, budget: &mut usize) -> Value {
    if *budget == 0 || depth > 6 {
        return json!({"omitted": true});
    }
    *budget -= 1;
    let key = key.to_ascii_lowercase();
    if secret_key(&key) {
        return Value::String("<redacted>".to_owned());
    }
    match value {
        Value::Object(object) => {
            let mut result = Map::new();
            for (name, value) in object.iter().take(48) {
                if *budget == 0 {
                    break;
                }
                let (captured_name, child_context) = field(context, name);
                result.insert(
                    captured_name,
                    capture(value, name, child_context, depth + 1, budget),
                );
            }
            if result.len() < object.len() {
                result.insert(
                    "_omittedFields".to_owned(),
                    json!(object.len() - result.len()),
                );
            }
            Value::Object(result)
        }
        Value::Array(items) => json!({
            "length": items.len(),
            "sample": items.iter().take(4)
                .map(|item| {
                    // 受控头值只接受一层字符串数组；畸形嵌套不能继承头部放行规则。
                    let context = if matches!(context, Context::HeaderValue) && !item.is_string() {
                        Context::Opaque
                    } else {
                        context
                    };
                    capture(item, key.as_str(), context, depth + 1, budget)
                })
                .collect::<Vec<_>>()
        }),
        Value::String(text)
            if matches!(context, Context::HeaderValue) && safe_header(&key)
                || matches!(context, Context::Protocol | Context::UpstreamEvent)
                    && protocol_value(&key, text) =>
        {
            Value::String(bounded(text, 256))
        }
        Value::String(text) => body_fingerprint(text.as_bytes()),
        Value::Number(number)
            if matches!(context, Context::Protocol)
                && key == "status"
                && number
                    .as_u64()
                    .is_some_and(|status| (100..=599).contains(&status)) =>
        {
            value.clone()
        }
        other => body_fingerprint(other.to_string().as_bytes()),
    }
}

fn field(context: Context, name: &str) -> (String, Context) {
    let child = match (context, name) {
        (Context::UpstreamEvent, "headers") => Some(Context::Headers),
        (Context::Headers, _) if safe_header(&name.to_ascii_lowercase()) => {
            Some(Context::HeaderValue)
        }
        (
            Context::Protocol | Context::UpstreamEvent,
            "response" | "error" | "input" | "output" | "item" | "part" | "content" | "type"
            | "role" | "status",
        ) => Some(Context::Protocol),
        (
            Context::Protocol | Context::UpstreamEvent,
            "id" | "model" | "message" | "text" | "delta" | "code" | "metadata" | "headers",
        ) => Some(Context::Opaque),
        _ => None,
    };
    match child {
        Some(context) => (name.to_owned(), context),
        None => (
            format!("field_{}", fingerprint_hex(name.as_bytes())),
            Context::Opaque,
        ),
    }
}

fn protocol_value(key: &str, value: &str) -> bool {
    match key {
        "type" => {
            known_event_type(value)
                || matches!(
                    value,
                    "message"
                        | "function_call"
                        | "function_call_output"
                        | "reasoning"
                        | "input_text"
                        | "output_text"
                        | "input_image"
                        | "refusal"
                )
        }
        "role" => matches!(
            value,
            "system" | "developer" | "user" | "assistant" | "tool"
        ),
        "status" => matches!(value, "in_progress" | "completed" | "incomplete" | "failed"),
        _ => false,
    }
}

/// 请求/响应头保持多值；安全 trace ID 原样保存，未知值只保存摘要。
#[must_use]
pub fn diagnostic_headers<'a>(headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    let mut result = Map::new();
    for (name, value) in headers.into_iter().take(64) {
        let name = bounded(&name.to_ascii_lowercase(), 96);
        let value = if secret_key(&name) {
            Value::String("<redacted>".to_owned())
        } else if safe_header(&name) {
            Value::String(bounded(value, 256))
        } else {
            body_fingerprint(value.as_bytes())
        };
        let values = result
            .entry(name)
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(values) = values {
            values.push(value);
        }
    }
    Value::Object(result)
}

fn secret_key(key: &str) -> bool {
    key.contains("authorization")
        || key.contains("cookie")
        || key.contains("api_key")
        || key.contains("api-key")
        || key.contains("token") && !key.ends_with("tokens")
        || key.contains("password")
        || key.contains("secret")
        || key.contains("attestation")
        || matches!(key, "x-oai-is" | "x-oai-is-update")
}

fn safe_header(key: &str) -> bool {
    matches!(
        key,
        "x-oai-request-id"
            | "x-client-request-id"
            | "x-request-id"
            | "request-id"
            | "openai-request-id"
            | "x-openai-request-id"
            | "cf-ray"
            | "traceparent"
            | "tracestate"
            | "content-type"
            | "content-length"
            | "content-encoding"
            | "retry-after"
            | "openai-processing-ms"
            | "x-processing-ms"
            | "date"
            | "server"
            | "sec-websocket-extensions"
            | "x-codex-allowed"
            | "x-codex-limit-reached"
            | "x-codex-active-limit"
            | "x-models-etag"
    )
}

pub(super) fn bounded(value: &str, limit: usize) -> String {
    value
        .chars()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(super) fn diagnostic_event_type(value: &str) -> Value {
    if known_event_type(value) {
        Value::String(value.to_owned())
    } else {
        body_fingerprint(value.as_bytes())
    }
}

/// 这里只决定诊断可明文展示的阶段，不校验协议；未知事件仍采集摘要，不能按字符形状放行。
fn known_event_type(value: &str) -> bool {
    matches!(
        value,
        "error"
            | "response.create"
            | "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.failed"
            | "response.incomplete"
            | "response.metadata"
            | "codex.response.metadata"
            | "codex.rate_limits"
            | "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.delta"
            | "response.output_text.done"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done"
    )
}
