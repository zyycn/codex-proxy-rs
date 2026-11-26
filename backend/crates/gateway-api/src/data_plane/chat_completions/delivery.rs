//! Chat 出口只编码已转换的错误，不另建正文或提交状态机

use bytes::Bytes;
use gateway_core::event::{GatewayEvent, ProviderEvent};
use gateway_protocol::openai::sse::{parse_sse_events, sse_frame_is_done};
use serde_json::Value;

pub(in crate::data_plane) fn chat_error_frame(data: &Value) -> Bytes {
    let error = data
        .pointer("/response/error")
        .or_else(|| data.get("error"))
        .unwrap_or(data);
    Bytes::from(format!("data: {}\n\n", serde_json::json!({"error": error})))
}

pub(in crate::data_plane) fn validate_response(bytes: &[u8]) -> Result<(), ()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    if value.get("object").and_then(Value::as_str) != Some("chat.completion")
        || value.get("id").and_then(Value::as_str).is_none()
        || !value
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                choices.len() == 1
                    && choices[0].get("message").is_some_and(Value::is_object)
                    && choices[0]
                        .get("finish_reason")
                        .is_some_and(Value::is_string)
            })
    {
        return Err(());
    }
    Ok(())
}

/// finish 与 usage 必须随原生 Completed 交付，避免终结失败后已向客户端报成功
#[derive(Default)]
pub(in crate::data_plane) struct ChatEncoder {
    terminal: Vec<Bytes>,
    pub(in crate::data_plane) completed: bool,
    pub(in crate::data_plane) failed: bool,
}

impl ChatEncoder {
    pub(in crate::data_plane) fn push(&mut self, event: &ProviderEvent) -> Vec<Bytes> {
        let mut output = Vec::new();
        if let Some(wire) = event
            .wire_event()
            .filter(|wire| wire.protocol() == gateway_protocol::chat_completions::CHAT_PROTOCOL)
        {
            self.failed |= wire.data().get("error").is_some_and(Value::is_object);
            let bytes = wire.raw_sse_frame().cloned().or_else(|| {
                wire.has_json_data()
                    .then(|| Bytes::from(format!("data: {}\n\n", wire.data())))
            });
            if let Some(bytes) = bytes {
                if wire
                    .data()
                    .pointer("/choices/0/finish_reason")
                    .is_some_and(Value::is_string)
                    || wire
                        .data()
                        .get("choices")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                {
                    self.terminal.push(bytes);
                } else {
                    output.push(bytes);
                }
            }
        }
        if event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
        {
            self.completed = true;
            output.append(&mut self.terminal);
        }
        output
    }
}

#[derive(Default)]
pub(in crate::data_plane) struct ChatDeliveryValidator {
    id: Option<String>,
    finished: bool,
    done: bool,
}

impl ChatDeliveryValidator {
    pub(in crate::data_plane) fn response_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(in crate::data_plane) fn validate_frame(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if self.done {
            return Err(());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ())?;
        if sse_frame_is_done(text) {
            if !self.finished {
                return Err(());
            }
            self.done = true;
            return Ok(());
        }
        for event in parse_sse_events(text).map_err(|_| ())? {
            let value: Value = serde_json::from_str(&event.data).map_err(|_| ())?;
            if value.get("error").is_some_and(Value::is_object) {
                self.finished = true;
                continue;
            }
            if value.get("object").and_then(Value::as_str) != Some("chat.completion.chunk") {
                return Err(());
            }
            let id = value.get("id").and_then(Value::as_str).ok_or(())?;
            // 首帧必须留有提交锚点，终态不能替代被中间件移除的首个业务帧
            if self.id.is_none()
                && value
                    .pointer("/choices/0/finish_reason")
                    .is_some_and(Value::is_string)
            {
                return Err(());
            }
            if self.id.as_deref().is_some_and(|expected| expected != id) {
                return Err(());
            }
            self.id.get_or_insert_with(|| id.to_owned());
            let choices = value.get("choices").and_then(Value::as_array).ok_or(())?;
            if choices.is_empty() {
                if !self.finished || !value.get("usage").is_some_and(Value::is_object) {
                    return Err(());
                }
                continue;
            }
            if self.finished
                || choices.len() != 1
                || choices[0].get("index").and_then(Value::as_u64) != Some(0)
            {
                return Err(());
            }
            self.finished = choices[0]
                .get("finish_reason")
                .is_some_and(Value::is_string);
        }
        Ok(())
    }

    pub(in crate::data_plane) fn finish_delivery(&self) -> Result<(), ()> {
        if self.finished && self.done {
            Ok(())
        } else {
            Err(())
        }
    }
}
