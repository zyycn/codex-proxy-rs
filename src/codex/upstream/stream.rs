use serde_json::{json, Value};

use crate::codex::transport::sse::parse_sse_events;

pub(crate) enum CollectedResponse {
    Completed(Value),
    Failed(ResponsesSseFailure),
    MissingCompleted,
}

#[derive(Debug, Clone)]
pub(crate) struct ResponsesSseFailure {
    event: String,
    message: String,
    upstream_code: Option<String>,
}

impl ResponsesSseFailure {
    fn from_event(event: &str, value: &Value) -> Self {
        Self {
            event: event.to_string(),
            message: failure_message(value).unwrap_or_else(|| "Codex upstream SSE failed".into()),
            upstream_code: failure_code(value),
        }
    }

    pub(crate) fn openai_error_message(&self) -> String {
        format!("Codex upstream SSE failed: {}", self.message)
    }

    pub(crate) fn metadata(&self, stream: bool) -> Value {
        let mut metadata = json!({"stream": stream});
        self.extend_metadata(&mut metadata);
        metadata
    }

    pub(super) fn extend_metadata(&self, metadata: &mut Value) {
        let Some(object) = metadata.as_object_mut() else {
            *metadata = self.metadata(true);
            return;
        };
        object.insert("failureEvent".to_string(), json!(self.event));
        object.insert("failureMessage".to_string(), json!(self.message));
        if let Some(code) = &self.upstream_code {
            object.insert("upstreamCode".to_string(), json!(code));
        }
    }
}

pub(crate) fn completed_response_json(
    body: &str,
) -> Result<CollectedResponse, crate::codex::transport::sse::SseError> {
    let events = parse_sse_events(body)?;
    let mut output_text = String::new();
    let mut output_items = Vec::new();
    let mut completed_response = None;
    let mut failed_response = None;
    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    output_text.push_str(delta);
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item") {
                    output_items.push(item.clone());
                }
            }
            Some("response.completed") => {
                if let Some(response) = value.get("response") {
                    completed_response = Some(response.clone());
                }
            }
            Some(event_name @ ("error" | "response.failed")) if failed_response.is_none() => {
                failed_response = Some(ResponsesSseFailure::from_event(event_name, &value));
            }
            _ => {}
        }
    }
    if let Some(failure) = failed_response {
        return Ok(CollectedResponse::Failed(failure));
    }
    let Some(mut response) = completed_response else {
        return Ok(CollectedResponse::MissingCompleted);
    };
    ensure_completed_response_output(&mut response, &output_items, &output_text);
    sync_output_text_from_output(&mut response);
    Ok(CollectedResponse::Completed(response))
}

pub(super) fn responses_sse_failure(
    body: &str,
) -> Result<Option<ResponsesSseFailure>, crate::codex::transport::sse::SseError> {
    for event in parse_sse_events(body)? {
        let Some(event_name @ ("error" | "response.failed")) = event.event.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        return Ok(Some(ResponsesSseFailure::from_event(event_name, &value)));
    }
    Ok(None)
}

pub(super) fn ensure_stream_metadata(metadata: &mut Value, stream_value: bool) {
    let Some(object) = metadata.as_object_mut() else {
        *metadata = json!({"stream": stream_value});
        return;
    };
    object
        .entry("stream".to_string())
        .or_insert_with(|| json!(stream_value));
}

fn failure_message(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(ToString::to_string)
}

fn failure_code(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .filter(|code| !code.trim().is_empty())
        .map(ToString::to_string)
}

fn ensure_completed_response_output(
    response: &mut Value,
    output_items: &[Value],
    output_text: &str,
) {
    let output_is_empty = response
        .get("output")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    if !output_is_empty {
        return;
    }

    if !output_items.is_empty() {
        response["output"] = Value::Array(output_items.to_vec());
        return;
    }
    if output_text.is_empty() {
        return;
    }

    response["output"] = json!([{
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": output_text,
            "annotations": []
        }]
    }]);
}

fn sync_output_text_from_output(response: &mut Value) {
    let output_text_is_empty = response
        .get("output_text")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty);
    if !output_text_is_empty {
        return;
    }
    let Some(items) = response.get("output").and_then(Value::as_array) else {
        return;
    };
    let texts = items
        .iter()
        .filter_map(output_text_from_item)
        .collect::<Vec<_>>();
    if texts.is_empty() {
        return;
    }
    response["output_text"] = Value::String(texts.join("\n\n"));
}

fn output_text_from_item(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|part| {
            let part_type = part.get("type")?.as_str()?;
            if part_type != "output_text" && part_type != "text" {
                return None;
            }
            part.get("text")?.as_str()
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}
