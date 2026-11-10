//! 文本、多模态内容和不透明回传载荷的协议表示

use serde_json::{Value, json};

const ANTHROPIC_CARRIER: &str = "cpr:anthropic:v1:";
const OPENAI_CARRIER: &str = "cpr:openai:v1:";
const REDACTED_CARRIER: &str = "claude-redacted-thinking:";

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("thinking"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(super) fn blocks(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::String(text)) => vec![json!({"type":"text","text":text})],
        Some(Value::Array(parts)) => parts.clone(),
        _ => Vec::new(),
    }
}

pub(super) fn to_responses_content(block: &Value, assistant: bool) -> Option<Value> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => Some(
            json!({"type": if assistant {"output_text"} else {"input_text"}, "text":block.get("text").and_then(Value::as_str).unwrap_or_default()}),
        ),
        "image" => {
            let source = block.get("source")?;
            let url = source_url(source)?;
            Some(json!({"type":"input_image","image_url":url}))
        }
        "document" => {
            let source = block.get("source")?;
            match source.get("type").and_then(Value::as_str)? {
                "text" => Some(
                    json!({"type":"input_text","text":source.get("data").and_then(Value::as_str).unwrap_or_default()}),
                ),
                "file" => Some(json!({"type":"input_file","file_id":source.get("file_id")?})),
                "url" => Some(json!({"type":"input_file","file_url":source.get("url")?})),
                "base64" => Some(
                    json!({"type":"input_file","filename":block.get("title").and_then(Value::as_str).unwrap_or("document.pdf"),"file_data":source_url(source)?}),
                ),
                _ => None,
            }
        }
        "search_result" => Some(
            json!({"type":"input_text","text":text(block.get("content").unwrap_or(&Value::Null))}),
        ),
        _ => None,
    }
}

pub(super) fn to_messages_content(part: &Value) -> Option<Value> {
    match part.get("type").and_then(Value::as_str)? {
        "input_text" | "output_text" | "text" => Some(
            json!({"type":"text","text":part.get("text").and_then(Value::as_str).unwrap_or_default()}),
        ),
        "refusal" => Some(
            json!({"type":"text","text":part.get("refusal").and_then(Value::as_str).unwrap_or_default()}),
        ),
        "input_image" => {
            let url = part
                .get("image_url")
                .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(Value::as_str)))?;
            Some(json!({"type":"image","source":url_source(url)}))
        }
        "input_file" => {
            let source = if let Some(data) = part.get("file_data").and_then(Value::as_str) {
                url_source(data)
            } else if let Some(url) = part.get("file_url").and_then(Value::as_str) {
                json!({"type":"url","url":url})
            } else {
                json!({"type":"file","file_id":part.get("file_id")?})
            };
            Some(json!({"type":"document","source":source}))
        }
        _ => None,
    }
}

pub(super) fn source_url(source: &Value) -> Option<String> {
    match source.get("type").and_then(Value::as_str)? {
        "url" => source.get("url").and_then(Value::as_str).map(str::to_owned),
        "base64" => Some(format!(
            "data:{};base64,{}",
            source.get("media_type").and_then(Value::as_str)?,
            source.get("data").and_then(Value::as_str)?
        )),
        _ => None,
    }
}

fn url_source(url: &str) -> Value {
    if let Some(data) = url.strip_prefix("data:")
        && let Some((media_type, data)) = data.split_once(";base64,")
    {
        return json!({"type":"base64","media_type":media_type,"data":data});
    }
    json!({"type":"url","url":url})
}

pub(super) fn reasoning_text(item: &Value) -> String {
    let summary = text(item.get("summary").unwrap_or(&Value::Null));
    if summary.is_empty() {
        text(item.get("content").unwrap_or(&Value::Null))
    } else {
        summary
    }
}

pub(super) fn anthropic_carrier(block: &Value) -> String {
    format!("{ANTHROPIC_CARRIER}{block}")
}

pub(super) fn openai_carrier(item: &Value) -> String {
    format!("{OPENAI_CARRIER}{item}")
}

pub(super) fn is_openai_carrier(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("thinking")
        && block
            .get("signature")
            .and_then(Value::as_str)
            .is_some_and(|signature| signature.starts_with(OPENAI_CARRIER))
}

pub(super) fn is_anthropic_carrier(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
        && item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|carrier| carrier.starts_with(ANTHROPIC_CARRIER))
}

pub(super) fn restore_anthropic(item: &Value) -> Option<Value> {
    let carrier = item.get("encrypted_content")?.as_str()?;
    if let Some(raw) = carrier.strip_prefix(ANTHROPIC_CARRIER) {
        return serde_json::from_str(raw).ok();
    }
    if let Some(data) = carrier.strip_prefix(REDACTED_CARRIER) {
        return Some(json!({"type":"redacted_thinking","data":data}));
    }
    // CPA 识别来源后回放签名，CPR 自己发出的载荷携带完整块以保留精确回传内容
    if carrier.starts_with("claude#") || carrier.starts_with("anthropic#") {
        return Some(
            json!({"type":"thinking","thinking":reasoning_text(item),"signature":carrier.split_once('#')?.1}),
        );
    }
    None
}

pub(super) fn restore_openai(block: &Value) -> Option<Value> {
    let signature = block.get("signature")?.as_str()?;
    if let Some(raw) = signature.strip_prefix(OPENAI_CARRIER) {
        return serde_json::from_str(raw).ok();
    }
    if signature.starts_with("gAAAA") {
        return Some(json!({"type":"reasoning","summary":[],"encrypted_content":signature}));
    }
    None
}
