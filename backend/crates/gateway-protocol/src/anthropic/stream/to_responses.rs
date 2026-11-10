//! Messages SSE 到 Responses SSE 的逐块转换

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::super::{
    TranslationError,
    response::{
        complete_response, messages_block_to_response, response_from_message,
        terminal_response_event,
    },
    tools::ToolSet,
};
use super::{
    MessagesAccumulator,
    accumulator::{Block, index},
};

/// 单次 Messages 上游执行对应的 Responses 转换状态
#[derive(Debug)]
pub struct MessagesToResponsesStream {
    model: Option<Value>,
    tools: ToolSet,
    accumulator: MessagesAccumulator,
    response_id: String,
    output_indices: BTreeMap<u64, usize>,
    sequence: u64,
}

impl MessagesToResponsesStream {
    /// 原始 Responses 请求用于工具身份与模型恢复
    #[must_use]
    pub fn new(original_request: &Map<String, Value>) -> Self {
        Self {
            model: original_request.get("model").cloned(),
            tools: ToolSet::identities(original_request),
            accumulator: MessagesAccumulator::default(),
            response_id: String::new(),
            output_indices: BTreeMap::new(),
            sequence: 0,
        }
    }

    /// JSON 和事件流共用同一份模型及工具身份，不持有客户端历史
    pub fn complete_json_event(&self, message: &Value) -> Result<Value, TranslationError> {
        complete_response(message, self.model.as_ref(), &self.tools).map(terminal_response_event)
    }

    /// 输入已解码事件，返回携带 type 的 Responses 事件
    pub fn push(&mut self, event: &Value) -> Result<Vec<Value>, TranslationError> {
        let terminal = self.accumulator.accumulate(event)?;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut events = Vec::new();
        match kind {
            "message_start" => {
                let message = event.get("message").ok_or(TranslationError::InvalidEvent(
                    "The message_start event must contain a message.",
                ))?;
                self.response_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("resp_messages")
                    .to_owned();
                let mut response = complete_response(message, self.model.as_ref(), &self.tools)?;
                response["status"] = json!("in_progress");
                response["output"] = json!([]);
                response["usage"] = Value::Null;
                events.push(json!({"type":"response.created","response":response}));
                events.push(json!({"type":"response.in_progress","response":response}));
            }
            "content_block_start" => {
                let index = index(event)?;
                if let Some(block) = self.accumulator.block(index)
                    && let Some(mut item) = self.block_item(index, block)
                {
                    let block = &block.value;
                    let output_index = self.output_indices.len();
                    self.output_indices.insert(index, output_index);
                    let item_type = item
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    item["status"] = json!("in_progress");
                    match item_type.as_str() {
                        "message" => item["content"] = json!([]),
                        "function_call" => item["arguments"] = json!(""),
                        "custom_tool_call" => item["input"] = json!(""),
                        "reasoning" => {
                            item["summary"] = json!([]);
                            if let Some(object) = item.as_object_mut() {
                                object.remove("encrypted_content");
                            }
                        }
                        _ => {}
                    }
                    events.push(json!({"type":"response.output_item.added","output_index":output_index,"item":item}));
                    if item_type == "message" {
                        events.push(json!({"type":"response.content_part.added","output_index":output_index,"item_id":self.item_id(index),"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}));
                        if let Some(text) = block
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                        {
                            events.push(self.delta(
                                index,
                                output_index,
                                "response.output_text.delta",
                                "delta",
                                text,
                            ));
                        }
                    } else if block.get("type").and_then(Value::as_str) == Some("thinking") {
                        events.push(json!({"type":"response.reasoning_summary_part.added","output_index":output_index,"item_id":self.item_id(index),"summary_index":0,"part":{"type":"summary_text","text":""}}));
                        if let Some(text) = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                        {
                            events.push(self.delta(
                                index,
                                output_index,
                                "response.reasoning_summary_text.delta",
                                "delta",
                                text,
                            ));
                        }
                    }
                }
            }
            "content_block_delta" => {
                let index = index(event)?;
                if let Some(output_index) = self.output_indices.get(&index).copied() {
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    let (kind, key) = match delta
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "text_delta" => ("response.output_text.delta", "text"),
                        "thinking_delta" => ("response.reasoning_summary_text.delta", "thinking"),
                        "input_json_delta" => {
                            ("response.function_call_arguments.delta", "partial_json")
                        }
                        _ => ("", ""),
                    };
                    let custom = self
                        .accumulator
                        .block(index)
                        .and_then(|block| block.value.get("name"))
                        .and_then(Value::as_str)
                        .and_then(|name| self.tools.tools.get(name))
                        .is_some_and(|tool| tool.custom);
                    if !kind.is_empty() && !(custom && key == "partial_json") {
                        events.push(self.delta(
                            index,
                            output_index,
                            kind,
                            "delta",
                            delta.get(key).and_then(Value::as_str).unwrap_or_default(),
                        ));
                    }
                }
            }
            "content_block_stop" => {
                let index = index(event)?;
                if let Some(output_index) = self.output_indices.get(&index).copied()
                    && let Some(block) = self.accumulator.block(index)
                    && !block.invalid_input
                    && let Some(item) = self.block_item(index, block)
                {
                    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "message" => {
                            let part = &item["content"][0];
                            events.push(self.delta(
                                index,
                                output_index,
                                "response.output_text.done",
                                "text",
                                part.get("text").and_then(Value::as_str).unwrap_or_default(),
                            ));
                            events.push(json!({"type":"response.content_part.done","output_index":output_index,"item_id":self.item_id(index),"content_index":0,"part":part}));
                        }
                        "function_call" => events.push(
                            self.delta(
                                index,
                                output_index,
                                "response.function_call_arguments.done",
                                "arguments",
                                item.get("arguments")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            ),
                        ),
                        "custom_tool_call" => {
                            let input = item
                                .get("input")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            events.push(self.delta(
                                index,
                                output_index,
                                "response.custom_tool_call_input.delta",
                                "delta",
                                input,
                            ));
                            events.push(self.delta(
                                index,
                                output_index,
                                "response.custom_tool_call_input.done",
                                "input",
                                input,
                            ));
                        }
                        "reasoning" => {
                            if let Some(part) = item
                                .get("summary")
                                .and_then(Value::as_array)
                                .and_then(|parts| parts.first())
                            {
                                events.push(self.delta(
                                    index,
                                    output_index,
                                    "response.reasoning_summary_text.done",
                                    "text",
                                    part.get("text").and_then(Value::as_str).unwrap_or_default(),
                                ));
                                events.push(json!({"type":"response.reasoning_summary_part.done","output_index":output_index,"item_id":self.item_id(index),"summary_index":0,"part":part}));
                            }
                        }
                        _ => {}
                    }
                    events.push(json!({"type":"response.output_item.done","output_index":output_index,"item":item}));
                }
            }
            "message_stop" | "error" => {
                if let Some(snapshot) = terminal {
                    let mut response =
                        response_from_message(&snapshot.message, self.model.as_ref(), Vec::new());
                    let mut output = Vec::new();
                    for (index, block) in &snapshot.blocks {
                        if block.invalid_input && block.value["type"] != "tool_use" {
                            return Err(TranslationError::InvalidEvent(
                                "The Messages tool input is not valid JSON.",
                            ));
                        }
                        if let Some(mut item) = self.block_item(*index, block) {
                            if block.invalid_input {
                                item["status"] = response["status"].clone();
                                let output_index = self.output_indices.get(index).copied().ok_or(
                                    TranslationError::InvalidEvent(
                                        "The tool output item is missing.",
                                    ),
                                )?;
                                let (event_type, field) = if item["type"] == "custom_tool_call" {
                                    events.push(self.delta(
                                        *index,
                                        output_index,
                                        "response.custom_tool_call_input.delta",
                                        "delta",
                                        item["input"].as_str().unwrap_or_default(),
                                    ));
                                    ("response.custom_tool_call_input.done", "input")
                                } else {
                                    ("response.function_call_arguments.done", "arguments")
                                };
                                events.push(self.delta(
                                    *index,
                                    output_index,
                                    event_type,
                                    field,
                                    item[field].as_str().unwrap_or_default(),
                                ));
                                events.push(json!({"type":"response.output_item.done","output_index":output_index,"item":item}));
                            }
                            output.push(item);
                        }
                    }
                    response["output"] = json!(output);
                    if kind == "error" {
                        response["id"] = json!(self.response_id);
                    }
                    events.push(terminal_response_event(response));
                }
            }
            _ => {}
        }
        for event in &mut events {
            event["sequence_number"] = json!(self.sequence);
            self.sequence += 1;
        }
        Ok(events)
    }

    /// 验证流收到了原生终态
    pub fn finish(&self) -> Result<(), TranslationError> {
        self.accumulator.finish()
    }

    fn item_id(&self, index: u64) -> String {
        format!("{}_{index}", self.response_id)
    }

    fn block_item(&self, index: u64, block: &Block) -> Option<Value> {
        let mut item = messages_block_to_response(&block.value, &self.tools, &self.item_id(index))?;
        if item["type"] == "function_call" && !block.input_json.is_empty() {
            item["arguments"] = json!(block.input_json);
        }
        if block.invalid_input && item["type"] == "custom_tool_call" {
            item["input"] = json!(super::super::tools::partial_custom_input(&block.input_json));
        }
        Some(item)
    }

    fn delta(&self, index: u64, output_index: usize, kind: &str, key: &str, text: &str) -> Value {
        let mut event =
            json!({"type":kind,"item_id":self.item_id(index),"output_index":output_index,key:text});
        if kind.contains("reasoning_summary") {
            event["summary_index"] = json!(0);
        } else if kind.contains("output_text") {
            event["content_index"] = json!(0);
        }
        event
    }
}
