//! Messages SSE 的完整响应聚合与终态校验

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::super::MessageError;

#[derive(Debug)]
struct Block {
    pub value: Value,
    pub input_json: String,
    pub invalid_input: bool,
    stopped: bool,
}

#[derive(Debug)]
struct MessageSnapshot {
    pub message: Value,
    pub blocks: BTreeMap<u64, Block>,
}

impl MessageSnapshot {
    fn into_json(mut self) -> Result<Value, MessageError> {
        if self.blocks.values().any(|block| block.invalid_input) {
            return Err(MessageError::InvalidEvent(
                "The Messages tool input is not valid JSON.",
            ));
        }
        if self.message.get("type").and_then(Value::as_str) != Some("error") {
            self.message["content"] =
                Value::Array(self.blocks.into_values().map(|block| block.value).collect());
        }
        Ok(self.message)
    }
}

/// 非流式调用的 Messages 聚合器，每个上游执行独立创建
#[derive(Debug, Default)]
pub struct MessagesAccumulator {
    message: Option<Value>,
    blocks: BTreeMap<u64, Block>,
    terminal: bool,
}

impl MessagesAccumulator {
    /// message_stop 返回完整响应，error 返回原生错误终态，未知事件不改变状态
    pub fn push(&mut self, event: &Value) -> Result<Option<Value>, MessageError> {
        self.accumulate(event)?
            .map(MessageSnapshot::into_json)
            .transpose()
    }

    // 原生 JSON 仅在完整工具输入可解析时交付
    fn accumulate(&mut self, event: &Value) -> Result<Option<MessageSnapshot>, MessageError> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or(MessageError::InvalidEvent(
                "The Messages event type is missing.",
            ))?;
        if self.terminal {
            return Err(MessageError::InvalidEvent(
                "The Messages stream contains events after its terminal event.",
            ));
        }
        match kind {
            "error" => {
                self.terminal = true;
                return Ok(Some(MessageSnapshot {
                    message: event.clone(),
                    blocks: BTreeMap::new(),
                }));
            }
            "message_start" => {
                if self.message.is_some() {
                    return Err(MessageError::InvalidEvent(
                        "The Messages stream contains duplicate message_start events.",
                    ));
                }
                let message = event.get("message").filter(|v| v.is_object()).ok_or(
                    MessageError::InvalidEvent(
                        "The message_start event must contain a message object.",
                    ),
                )?;
                self.message = Some(message.clone());
            }
            "content_block_start" => {
                self.require_message()?;
                let index = index(event)?;
                if self.blocks.contains_key(&index) {
                    return Err(MessageError::InvalidEvent(
                        "The Messages content block index is duplicated.",
                    ));
                }
                let value = event
                    .get("content_block")
                    .filter(|v| v.is_object())
                    .ok_or(MessageError::InvalidEvent(
                        "The content_block_start event must contain a content block.",
                    ))?
                    .clone();
                self.blocks.insert(
                    index,
                    Block {
                        value,
                        input_json: String::new(),
                        invalid_input: false,
                        stopped: false,
                    },
                );
            }
            "content_block_delta" => {
                let block = self.active_block(event)?;
                let delta = event.get("delta").ok_or(MessageError::InvalidEvent(
                    "The content block delta is missing.",
                ))?;
                match delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text_delta" => append(&mut block.value, "text", delta.get("text")),
                    "thinking_delta" => append(&mut block.value, "thinking", delta.get("thinking")),
                    "signature_delta" => {
                        append(&mut block.value, "signature", delta.get("signature"))
                    }
                    "input_json_delta" => block.input_json.push_str(
                        delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    "citations_delta" => {
                        if block.value.get("citations").is_none() {
                            block.value["citations"] = json!([]);
                        }
                        if let Some(citation) = delta.get("citation")
                            && let Some(citations) = block
                                .value
                                .get_mut("citations")
                                .and_then(Value::as_array_mut)
                        {
                            citations.push(citation.clone());
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let block = self.active_block(event)?;
                if !block.input_json.is_empty() {
                    match serde_json::from_str(&block.input_json) {
                        Ok(input) => block.value["input"] = input,
                        Err(_) => block.invalid_input = true,
                    }
                }
                block.stopped = true;
            }
            "message_delta" => {
                let message = self.message.as_mut().ok_or(MessageError::InvalidEvent(
                    "The Messages delta arrived before message_start.",
                ))?;
                if let Some(delta) = event.get("delta").and_then(Value::as_object) {
                    for (key, value) in delta {
                        message[key] = value.clone();
                    }
                }
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    if !message.get("usage").is_some_and(Value::is_object) {
                        message["usage"] = json!({});
                    }
                    for (key, value) in usage {
                        message["usage"][key] = value.clone();
                    }
                }
            }
            "message_stop" => {
                self.require_message()?;
                if self.blocks.values().any(|block| !block.stopped) {
                    return Err(MessageError::InvalidEvent(
                        "The Messages stream ended with an open content block.",
                    ));
                }
                let message = self.message.take().ok_or(MessageError::IncompleteStream)?;
                self.terminal = true;
                return Ok(Some(MessageSnapshot {
                    message,
                    blocks: std::mem::take(&mut self.blocks),
                }));
            }
            _ => {}
        }
        Ok(None)
    }

    /// 必须消费成功或失败终态，连接结束不能被当作成功
    pub fn finish(&self) -> Result<(), MessageError> {
        if self.terminal {
            Ok(())
        } else {
            Err(MessageError::IncompleteStream)
        }
    }

    fn require_message(&self) -> Result<(), MessageError> {
        if self.message.is_some() {
            Ok(())
        } else {
            Err(MessageError::InvalidEvent(
                "The Messages content arrived before message_start.",
            ))
        }
    }

    fn active_block(&mut self, event: &Value) -> Result<&mut Block, MessageError> {
        self.blocks
            .get_mut(&index(event)?)
            .filter(|block| !block.stopped)
            .ok_or(MessageError::InvalidEvent(
                "The Messages delta references a content block that is not open.",
            ))
    }
}

fn index(event: &Value) -> Result<u64, MessageError> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .ok_or(MessageError::InvalidEvent(
            "The content block index is missing.",
        ))
}

fn append(block: &mut Value, key: &str, fragment: Option<&Value>) {
    if let Some(fragment) = fragment.and_then(Value::as_str) {
        if let Some(Value::String(value)) = block.get_mut(key) {
            value.push_str(fragment);
        } else {
            block[key] = Value::String(fragment.to_owned());
        }
    }
}
