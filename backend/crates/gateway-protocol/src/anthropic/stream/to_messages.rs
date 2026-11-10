//! Responses SSE 到 Messages SSE 的增量转换与签名终态回传

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::super::{
    TranslationError, content,
    response::{responses_error_to_messages, responses_item_to_messages, responses_stop_reason},
    usage,
};

#[derive(Debug)]
struct Block {
    index: usize,
    kind: String,
    emitted: String,
    signature_sent: bool,
    stopped: bool,
    last_summary_index: Option<u64>,
}

/// 每次 Responses 执行独立持有输出块与结束状态
#[derive(Debug)]
pub struct ResponsesToMessagesStream {
    model: Option<Value>,
    blocks: BTreeMap<(u64, usize), Block>,
    started: bool,
    terminal: bool,
}

impl ResponsesToMessagesStream {
    /// 原始 Messages 请求用于保留客户端模型身份
    #[must_use]
    pub fn new(original_request: &Map<String, Value>) -> Self {
        Self {
            model: original_request.get("model").cloned(),
            blocks: BTreeMap::new(),
            started: false,
            terminal: false,
        }
    }

    /// 完整 JSON 响应只需要客户端模型名
    pub fn complete_json(&self, response: &Value) -> Result<Value, TranslationError> {
        super::super::response::complete_message(response, self.model.as_ref())
    }

    /// 输入已解码 Responses 事件，返回 Messages 事件
    pub fn push(&mut self, event: &Value) -> Result<Vec<Value>, TranslationError> {
        if self.terminal {
            return Err(TranslationError::InvalidEvent(
                "The Responses stream contains events after its terminal event.",
            ));
        }
        let kind =
            event
                .get("type")
                .and_then(Value::as_str)
                .ok_or(TranslationError::InvalidEvent(
                    "The Responses event type is missing.",
                ))?;
        let mut events = Vec::new();
        if matches!(kind, "error" | "response.failed") {
            let error = event.get("error").or_else(||event.pointer("/response/error")).cloned().unwrap_or_else(||json!({"type":"api_error","message":event.get("message").and_then(Value::as_str).unwrap_or("The upstream response failed.")}));
            self.terminal = true;
            return Ok(vec![
                json!({"type":"error","error":responses_error_to_messages(&error)}),
            ]);
        }
        if kind.starts_with("response.") {
            self.start(event.get("response"), &mut events);
        }
        let output_index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let content_index = event
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize;
        match kind {
            "response.output_item.added" => {
                let item = event.get("item").filter(|v| v.is_object()).ok_or(
                    TranslationError::InvalidEvent("The output item event must contain an item."),
                )?;
                match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                    "function_call" | "custom_tool_call" | "reasoning" => {
                        if let Some(block) = responses_item_to_messages(item).first() {
                            self.ensure_block((output_index, 0), block, &mut events);
                        }
                    }
                    _ => {}
                }
            }
            "response.content_part.added" => {
                if let Some(part) = event.get("part").and_then(content::to_messages_content) {
                    self.ensure_block((output_index, content_index), &part, &mut events);
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let key = (output_index, content_index);
                self.ensure_block(key, &json!({"type":"text","text":""}), &mut events);
                self.emit_delta(
                    key,
                    "text_delta",
                    "text",
                    event
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &mut events,
                )?;
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let key = (output_index, 0);
                self.ensure_block(key, &json!({"type":"thinking","thinking":""}), &mut events);
                let part_index = event
                    .get("summary_index")
                    .or_else(|| event.get("content_index"))
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let separator = self.blocks.get(&key).is_some_and(|block| {
                    !block.emitted.is_empty()
                        && block
                            .last_summary_index
                            .is_some_and(|previous| previous != part_index)
                });
                if separator {
                    self.emit_delta(key, "thinking_delta", "thinking", "\n", &mut events)?;
                }
                if let Some(block) = self.blocks.get_mut(&key) {
                    block.last_summary_index = Some(part_index);
                }
                self.emit_delta(
                    key,
                    "thinking_delta",
                    "thinking",
                    event
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &mut events,
                )?;
            }
            "response.function_call_arguments.delta" => {
                let key = (output_index, 0);
                if !self.blocks.contains_key(&key) {
                    return Err(TranslationError::InvalidEvent(
                        "The tool arguments arrived before the output item.",
                    ));
                }
                self.emit_delta(
                    key,
                    "input_json_delta",
                    "partial_json",
                    event
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &mut events,
                )?;
            }
            // custom 输入在完整 item 中一次包装为 JSON，不能逐片转义成独立参数
            "response.custom_tool_call_input.delta" => {
                if !self.blocks.contains_key(&(output_index, 0)) {
                    return Err(TranslationError::InvalidEvent(
                        "The custom tool input arrived before the output item.",
                    ));
                }
            }
            "response.output_item.done" => {
                let item = event.get("item").filter(|v| v.is_object()).ok_or(
                    TranslationError::InvalidEvent("The output item event must contain an item."),
                )?;
                self.complete_item(output_index, item, false, &mut events)?;
            }
            "response.completed" | "response.incomplete" => {
                let response = event
                    .get("response")
                    .filter(|response| response.is_object())
                    .ok_or(TranslationError::InvalidEvent(
                        "The terminal Responses event must contain a response.",
                    ))?;
                if let Some(output) = response.get("output") {
                    let output = output.as_array().ok_or(TranslationError::InvalidEvent(
                        "The Responses output must be an array.",
                    ))?;
                    for (index, item) in output.iter().enumerate() {
                        self.complete_item(index as u64, item, true, &mut events)?;
                    }
                }
                let open = self.blocks.values_mut().filter(|block| !block.stopped);
                for block in open {
                    block.stopped = true;
                    events.push(json!({"type":"content_block_stop","index":block.index}));
                }
                if !response["error"].is_null() || response["status"] == "failed" {
                    events.push(json!({"type":"error","error":responses_error_to_messages(&response["error"])}));
                } else {
                    // 终态可以省略已经交付的 output，停止原因由客户端实际收到的块决定
                    let has_tool = self.blocks.values().any(|block| block.kind == "tool_use");
                    events.push(json!({"type":"message_delta","delta":{"stop_reason":responses_stop_reason(response,has_tool),"stop_sequence":null},"usage":usage::responses_usage(&response["usage"])}));
                    events.push(json!({"type":"message_stop"}));
                }
                self.terminal = true;
            }
            _ => {}
        }
        Ok(events)
    }

    /// 连接关闭仅在已消费终态时成功
    pub fn finish(&self) -> Result<(), TranslationError> {
        if self.terminal {
            Ok(())
        } else {
            Err(TranslationError::IncompleteStream)
        }
    }

    fn start(&mut self, response: Option<&Value>, events: &mut Vec<Value>) {
        if self.started {
            return;
        }
        let response = response.unwrap_or(&Value::Null);
        events.push(json!({"type":"message_start","message":{"id":response.get("id").and_then(Value::as_str).unwrap_or("msg_responses"),"type":"message","role":"assistant","model":self.model.as_ref().or_else(||response.get("model")),"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}));
        self.started = true;
    }

    fn ensure_block(&mut self, key: (u64, usize), block: &Value, events: &mut Vec<Value>) {
        if self.blocks.contains_key(&key) {
            return;
        }
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("text");
        let mut start = block.clone();
        match kind {
            "text" => start["text"] = json!(""),
            "thinking" => {
                start["thinking"] = json!("");
                if let Some(object) = start.as_object_mut() {
                    object.remove("signature");
                }
            }
            "tool_use" => start["input"] = json!({}),
            _ => {}
        }
        let index = self.blocks.len();
        self.blocks.insert(
            key,
            Block {
                index,
                kind: kind.to_owned(),
                emitted: String::new(),
                signature_sent: false,
                stopped: false,
                last_summary_index: None,
            },
        );
        events.push(json!({"type":"content_block_start","index":index,"content_block":start}));
    }

    fn emit_delta(
        &mut self,
        key: (u64, usize),
        kind: &str,
        field: &str,
        fragment: &str,
        events: &mut Vec<Value>,
    ) -> Result<(), TranslationError> {
        let block = self
            .blocks
            .get_mut(&key)
            .filter(|block| !block.stopped)
            .ok_or(TranslationError::InvalidEvent(
                "The Responses delta references a closed content block.",
            ))?;
        block.emitted.push_str(fragment);
        if !fragment.is_empty() {
            events.push(json!({"type":"content_block_delta","index":block.index,"delta":{"type":kind,field:fragment}}));
        }
        Ok(())
    }

    fn complete_item(
        &mut self,
        output_index: u64,
        item: &Value,
        terminal: bool,
        events: &mut Vec<Value>,
    ) -> Result<(), TranslationError> {
        let contents = if item.get("type").and_then(Value::as_str) == Some("message") {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, part)| {
                    content::to_messages_content(part).map(|content| (index, content))
                })
                .collect::<Vec<_>>()
        } else {
            responses_item_to_messages(item)
                .into_iter()
                .enumerate()
                .collect()
        };
        for (content_index, content) in &contents {
            let key = (output_index, *content_index);
            self.ensure_block(key, content, events);
            let Some(block) = self.blocks.get(&key) else {
                continue;
            };
            if block.stopped {
                continue;
            }
            let full = match block.kind.as_str() {
                "text" => content
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                "thinking" => content
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                "tool_use" => content
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string(),
                _ => String::new(),
            };
            let delta = full
                .strip_prefix(&block.emitted)
                .unwrap_or_default()
                .to_owned();
            let (kind, field) = match block.kind.as_str() {
                "text" => ("text_delta", "text"),
                "thinking" => ("thinking_delta", "thinking"),
                _ => ("input_json_delta", "partial_json"),
            };
            if !delta.is_empty() {
                self.emit_delta(key, kind, field, &delta, events)?;
            }
            let Some(block) = self.blocks.get_mut(&key) else {
                continue;
            };
            if block.kind == "thinking" && !block.signature_sent {
                if item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some()
                    || terminal
                {
                    events.push(json!({"type":"content_block_delta","index":block.index,"delta":{"type":"signature_delta","signature":content::openai_carrier(item)}}));
                    block.signature_sent = true;
                } else {
                    continue;
                }
            }
            block.stopped = true;
            events.push(json!({"type":"content_block_stop","index":block.index}));
        }
        Ok(())
    }
}
