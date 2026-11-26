//! Messages 事件直接产生 Chat chunk，独立维护原生块终态

use super::response::{annotation, finish_reason, usage};
use crate::anthropic::MessageUsage;
use crate::chat_completions::{
    ChatError, ChatIdentity,
    response::{chunk, custom_tools, usage_chunk},
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

struct Block {
    kind: String,
    stopped: bool,
    tool_index: Option<usize>,
    custom: bool,
    arguments: String,
    initial_input: Value,
    text_start: usize,
}

/// 仅持有本次 Messages 流的内容块与累计用量状态
pub struct ChatStreamEncoder {
    identity: ChatIdentity,
    include_usage: bool,
    custom: BTreeSet<String>,
    blocks: BTreeMap<u64, Block>,
    calls: BTreeSet<String>,
    started: bool,
    completed: bool,
    failed: bool,
    reason: Option<&'static str>,
    usage: MessageUsage,
    text_len: usize,
}

impl ChatStreamEncoder {
    pub fn new(identity: ChatIdentity, include_usage: bool) -> Self {
        Self {
            identity,
            include_usage,
            custom: BTreeSet::new(),
            blocks: BTreeMap::new(),
            calls: BTreeSet::new(),
            started: false,
            completed: false,
            failed: false,
            reason: None,
            usage: MessageUsage::default(),
            text_len: 0,
        }
    }

    pub fn with_request(
        identity: ChatIdentity,
        include_usage: bool,
        request: &Map<String, Value>,
    ) -> Self {
        Self {
            custom: custom_tools(request),
            ..Self::new(identity, include_usage)
        }
    }

    pub fn is_completed(&self) -> bool {
        self.completed
    }
    pub fn has_wire_failure(&self) -> bool {
        self.failed
    }

    pub fn push_event(
        &mut self,
        event: Option<&str>,
        value: &Value,
    ) -> Result<Vec<Value>, ChatError> {
        if self.failed {
            return Ok(Vec::new());
        }
        if self.completed {
            return Err(ChatError::InvalidResponse);
        }
        let kind = event
            .or_else(|| value.get("type").and_then(Value::as_str))
            .ok_or(ChatError::InvalidResponse)?;
        if let Some(data_kind) = value.get("type").and_then(Value::as_str)
            && data_kind != kind
        {
            return Err(ChatError::InvalidResponse);
        }
        if kind == "error" {
            let error = value
                .get("error")
                .filter(|v| v.is_object())
                .ok_or(ChatError::InvalidResponse)?;
            self.failed = true;
            return Ok(vec![json!({"error":error})]);
        }
        if kind == "ping" {
            return Ok(Vec::new());
        }
        if kind == "message_start" {
            if self.started {
                return Err(ChatError::InvalidResponse);
            }
            let message = &value["message"];
            if message["type"] != "message"
                || message["role"] != "assistant"
                || !message["content"].as_array().is_some_and(Vec::is_empty)
            {
                return Err(ChatError::InvalidResponse);
            }
            self.started = true;
            self.usage.merge(&message["usage"]);
            return Ok(vec![chunk(
                &self.identity,
                json!({"role":"assistant","content":"","refusal":null}),
                None,
                self.include_usage,
            )]);
        }
        if !self.started {
            return Err(ChatError::InvalidResponse);
        }
        let mut deltas = Vec::new();
        match kind {
            "content_block_start" => {
                if self.reason.is_some() {
                    return Err(ChatError::InvalidResponse);
                }
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or(ChatError::InvalidResponse)?;
                if self.blocks.contains_key(&index) {
                    return Err(ChatError::InvalidResponse);
                }
                let block = &value["content_block"];
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or(ChatError::InvalidResponse)?;
                let mut state = Block {
                    kind: kind.into(),
                    stopped: false,
                    tool_index: None,
                    custom: false,
                    arguments: String::new(),
                    initial_input: Value::Null,
                    text_start: self.text_len,
                };
                match kind {
                    "text" => {
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or(ChatError::InvalidResponse)?;
                        self.text_len += text.chars().count();
                        if !text.is_empty() {
                            deltas.push(json!({"content":text}));
                        }
                        for citation in block
                            .get("citations")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            if let Some(citation) =
                                annotation(citation, state.text_start, self.text_len)
                            {
                                deltas.push(json!({"annotations":[citation]}));
                            }
                        }
                    }
                    "thinking" => {
                        let text = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .ok_or(ChatError::InvalidResponse)?;
                        if !text.is_empty() {
                            deltas.push(json!({"reasoning_content":text}));
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                            .ok_or(ChatError::InvalidResponse)?;
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                            .ok_or(ChatError::InvalidResponse)?;
                        let tool_index = self.calls.len();
                        if !self.calls.insert(id.into()) || !block["input"].is_object() {
                            return Err(ChatError::InvalidResponse);
                        }
                        state.tool_index = Some(tool_index);
                        state.custom = self.custom.contains(name);
                        state.initial_input = block["input"].clone();
                        let call = if state.custom {
                            json!({"index":tool_index,"id":id,"type":"custom","custom":{"name":name,"input":""}})
                        } else {
                            json!({"index":tool_index,"id":id,"type":"function","function":{"name":name,"arguments":""}})
                        };
                        deltas.push(json!({"tool_calls":[call]}));
                    }
                    "redacted_thinking" | "server_tool_use" | "web_search_tool_result" => {}
                    _ => return Err(ChatError::InvalidResponse),
                }
                self.blocks.insert(index, state);
            }
            "content_block_delta" => {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or(ChatError::InvalidResponse)?;
                let block = self
                    .blocks
                    .get_mut(&index)
                    .filter(|v| !v.stopped)
                    .ok_or(ChatError::InvalidResponse)?;
                let delta = &value["delta"];
                match delta.get("type").and_then(Value::as_str).ok_or(ChatError::InvalidResponse)? {
                    "text_delta" if block.kind == "text" => { let text = delta.get("text").and_then(Value::as_str).ok_or(ChatError::InvalidResponse)?; self.text_len += text.chars().count(); deltas.push(json!({"content":text})); }
                    "thinking_delta" if block.kind == "thinking" => deltas.push(json!({"reasoning_content":delta.get("thinking").and_then(Value::as_str).ok_or(ChatError::InvalidResponse)?})),
                    "signature_delta" if block.kind == "thinking" => {},
                    "input_json_delta" if matches!(block.kind.as_str(), "tool_use" | "server_tool_use") => {
                        let arguments = delta.get("partial_json").and_then(Value::as_str).ok_or(ChatError::InvalidResponse)?;
                        block.arguments.push_str(arguments);
                        if !block.custom && let Some(index) = block.tool_index { deltas.push(json!({"tool_calls":[{"index":index,"function":{"arguments":arguments}}]})); }
                    }
                    "citations_delta" if block.kind == "text" => { if let Some(citation) = annotation(&delta["citation"], block.text_start, self.text_len) { deltas.push(json!({"annotations":[citation]})); } }
                    _ => return Err(ChatError::InvalidResponse),
                }
            }
            "content_block_stop" => {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or(ChatError::InvalidResponse)?;
                let block = self
                    .blocks
                    .get_mut(&index)
                    .filter(|v| !v.stopped)
                    .ok_or(ChatError::InvalidResponse)?;
                if let Some(index) = block.tool_index {
                    let input = if block.arguments.is_empty() {
                        block.initial_input.clone()
                    } else {
                        serde_json::from_str::<Value>(&block.arguments)
                            .map_err(|_| ChatError::InvalidResponse)?
                    };
                    if !input.is_object() {
                        return Err(ChatError::InvalidResponse);
                    }
                    if block.custom {
                        deltas.push(json!({"tool_calls":[{"index":index,"custom":{"input":input.get("input").and_then(Value::as_str).ok_or(ChatError::InvalidResponse)?}}]}));
                    } else if block.arguments.is_empty() {
                        deltas.push(json!({"tool_calls":[{"index":index,"function":{"arguments":input.to_string()}}]}));
                    }
                }
                block.stopped = true;
            }
            "message_delta" => {
                if self.blocks.values().any(|v| !v.stopped) {
                    return Err(ChatError::InvalidResponse);
                }
                if let Some(reason) = value.pointer("/delta/stop_reason").filter(|v| !v.is_null()) {
                    if self.reason.is_some() {
                        return Err(ChatError::InvalidResponse);
                    }
                    self.reason = Some(finish_reason(
                        reason.as_str().ok_or(ChatError::InvalidResponse)?,
                    )?);
                }
                self.usage.merge(&value["usage"]);
            }
            "message_stop" => {
                if self.blocks.values().any(|v| !v.stopped) {
                    return Err(ChatError::InvalidResponse);
                }
                let reason = self.reason.ok_or(ChatError::InvalidResponse)?;
                self.completed = true;
                let mut frames = vec![chunk(
                    &self.identity,
                    json!({}),
                    Some(reason),
                    self.include_usage,
                )];
                if self.include_usage
                    && let Some(usage) = usage(self.usage)
                {
                    frames.push(usage_chunk(&self.identity, usage));
                }
                return Ok(frames);
            }
            _ => {}
        }
        Ok(deltas
            .into_iter()
            .map(|delta| chunk(&self.identity, delta, None, self.include_usage))
            .collect())
    }

    pub fn finish(&mut self) -> Result<Vec<Value>, ChatError> {
        if self.completed || self.failed {
            Ok(Vec::new())
        } else {
            Err(ChatError::IncompleteStream)
        }
    }
}
