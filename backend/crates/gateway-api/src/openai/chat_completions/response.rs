//! Responses wire 到 Chat 消息的有状态投影；仅公开推理摘要进入 Chat 输出。

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use gateway_core::engine::ModelRequestId;
use gateway_core::event::{GatewayEvent, ProviderEvent};
use gateway_protocol::openai::events::ResponsesUsageTracker;
use serde_json::{Value, json};

use crate::openai::responses::{ProtocolError, ProtocolErrorBody};

#[derive(Default)]
struct Tool {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

pub(in crate::openai) struct ChatEncoder {
    id: String,
    created: u64,
    model: String,
    include_usage: bool,
    legacy_functions: bool,
    role_sent: bool,
    text: BTreeMap<(u64, u64), String>,
    refusal: BTreeMap<(u64, u64), String>,
    reasoning: BTreeMap<(u64, u64), String>,
    item_ids: BTreeMap<String, u64>,
    tools: BTreeMap<u64, Tool>,
    finish_reason: Option<&'static str>,
    usage: Option<Value>,
    usage_tracker: ResponsesUsageTracker,
    terminal_frames: Vec<Bytes>,
    wire_failure: bool,
    canonical_completed: bool,
}

impl ChatEncoder {
    pub(in crate::openai) fn new(
        id: &ModelRequestId,
        created: SystemTime,
        model: String,
        include_usage: bool,
    ) -> Self {
        Self {
            id: format!("chatcmpl-{}", id.as_str()),
            created: created
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model,
            include_usage,
            legacy_functions: false,
            role_sent: false,
            text: BTreeMap::new(),
            refusal: BTreeMap::new(),
            reasoning: BTreeMap::new(),
            item_ids: BTreeMap::new(),
            tools: BTreeMap::new(),
            finish_reason: None,
            usage: None,
            usage_tracker: ResponsesUsageTracker::default(),
            terminal_frames: Vec::new(),
            wire_failure: false,
            canonical_completed: false,
        }
    }

    pub(in crate::openai) fn with_legacy_functions(mut self, enabled: bool) -> Self {
        self.legacy_functions = enabled;
        self
    }

    pub(in crate::openai) fn is_completed(&self) -> bool {
        self.finish_reason.is_some()
    }

    pub(in crate::openai) fn has_wire_failure(&self) -> bool {
        self.wire_failure
    }

    pub(in crate::openai) fn push_sse(
        &mut self,
        event: &ProviderEvent,
    ) -> Result<Vec<Bytes>, ProtocolErrorBody> {
        self.canonical_completed |= event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
        let mut frames = Vec::new();
        // 首批只有 created、推理或心跳时也给 Core 一个可提交的角色帧。
        if !self.role_sent {
            self.role_sent = true;
            frames.push(self.delta(json!({"role":"assistant","content":""})));
        }
        let Some(wire) = event
            .wire_event()
            .filter(|wire| wire.protocol() == "openai" && wire.has_json_data())
        else {
            return Ok(frames);
        };
        let data = wire.data();
        self.usage_tracker.observe(data);
        let event_type = wire
            .event_type()
            .or_else(|| data.get("type").and_then(Value::as_str));
        if self.is_completed() || self.wire_failure {
            return Err(invalid_response());
        }
        match event_type {
            Some("response.reasoning_summary_text.delta") => {
                let key = (
                    required_index(data, "output_index")?,
                    required_index(data, "summary_index")?,
                );
                let value = required_str(data, "delta")?;
                self.reasoning.entry(key).or_default().push_str(value);
                frames.push(self.delta(json!({"reasoning_content": value})));
            }
            Some("response.reasoning_summary_text.done") => {
                self.reasoning_snapshot(
                    (
                        required_index(data, "output_index")?,
                        required_index(data, "summary_index")?,
                    ),
                    required_str(data, "text")?,
                    &mut frames,
                )?;
            }
            Some("response.reasoning_summary_part.done") => {
                let part = data.get("part").ok_or_else(invalid_response)?;
                if part.get("type").and_then(Value::as_str) == Some("summary_text") {
                    self.reasoning_snapshot(
                        (
                            required_index(data, "output_index")?,
                            required_index(data, "summary_index")?,
                        ),
                        required_str(part, "text")?,
                        &mut frames,
                    )?;
                }
            }
            Some("response.output_text.delta" | "response.refusal.delta") => {
                let key = content_key(data)?;
                let value = required_str(data, "delta")?;
                let refusal = event_type == Some("response.refusal.delta");
                let field = if refusal { "refusal" } else { "content" };
                let content = if refusal {
                    &mut self.refusal
                } else {
                    &mut self.text
                };
                content.entry(key).or_default().push_str(value);
                frames.push(self.delta(json!({field:value})));
            }
            Some("response.output_text.done" | "response.refusal.done") => {
                let refusal = event_type == Some("response.refusal.done");
                let value = required_str(data, if refusal { "refusal" } else { "text" })?;
                self.text_snapshot(content_key(data)?, value, refusal, &mut frames)?;
            }
            Some("response.content_part.done") => {
                self.part(
                    content_key(data)?,
                    data.get("part").ok_or_else(invalid_response)?,
                    &mut frames,
                )?;
            }
            Some("response.output_item.added" | "response.output_item.done") => {
                self.item(
                    required_index(data, "output_index")?,
                    data.get("item").ok_or_else(invalid_response)?,
                    &mut frames,
                )?;
            }
            Some("response.function_call_arguments.delta") => {
                let index = required_index(data, "output_index")?;
                let delta = required_str(data, "delta")?;
                let tool = self.tools.get_mut(&index).ok_or_else(invalid_response)?;
                tool.arguments.push_str(delta);
                let tool_index = tool.index;
                frames.push(self.tool_delta(tool_index, None, None, delta));
            }
            Some("response.function_call_arguments.done") => {
                let index = required_index(data, "output_index")?;
                if let Some(arguments) = data.get("arguments") {
                    self.arguments_snapshot(
                        index,
                        arguments.as_str().ok_or_else(invalid_response)?,
                        &mut frames,
                    )?;
                } else if !self.tools.contains_key(&index) {
                    return Err(invalid_response());
                }
            }
            Some("response.completed" | "response.done" | "response.incomplete") => {
                let response = data
                    .get("response")
                    .filter(|value| value.is_object())
                    .ok_or_else(invalid_response)?;
                let status = required_str(response, "status")?;
                if !matches!(status, "completed" | "incomplete")
                    || (event_type == Some("response.incomplete") && status != "incomplete")
                {
                    return Err(invalid_response());
                }
                let mut terminal_frames = Vec::new();
                if let Some(output) = response.get("output") {
                    for (index, item) in output
                        .as_array()
                        .ok_or_else(invalid_response)?
                        .iter()
                        .enumerate()
                    {
                        self.item(
                            u64::try_from(index).map_err(|_| invalid_response())?,
                            item,
                            &mut terminal_frames,
                        )?;
                    }
                } else if self.text.is_empty()
                    && self.refusal.is_empty()
                    && self.tools.is_empty()
                    && self.reasoning.is_empty()
                {
                    return Err(invalid_response());
                }
                self.usage = self
                    .usage_tracker
                    .selected(data)
                    .map(chat_usage)
                    .transpose()?;
                self.finish_reason = Some(
                    if event_type == Some("response.incomplete")
                        || response.get("status").and_then(Value::as_str) == Some("incomplete")
                    {
                        match response
                            .pointer("/incomplete_details/reason")
                            .and_then(Value::as_str)
                        {
                            Some("max_output_tokens" | "max_tokens") => "length",
                            Some("content_filter") => "content_filter",
                            _ => return Err(invalid_response()),
                        }
                    } else if self.tools.is_empty() {
                        "stop"
                    } else if self.legacy_functions {
                        "function_call"
                    } else {
                        "tool_calls"
                    },
                );
                // 终态快照补出的内容与 finish/usage 必须在执行终结成功后一起交付。
                self.terminal_frames = terminal_frames;
            }
            Some("response.failed" | "error") => {
                let error = data
                    .pointer("/response/error")
                    .or_else(|| data.get("error"))
                    .unwrap_or(data);
                let message = required_str(error, "message")?;
                self.wire_failure = true;
                frames.push(frame(&json!({"error": {
                    "message": message,
                    "type": error.get("type").and_then(Value::as_str).unwrap_or("server_error"),
                    "code": error.get("code").cloned().unwrap_or(Value::Null),
                    "param": error.get("param").cloned().unwrap_or(Value::Null),
                }})));
            }
            _ => {}
        }
        Ok(frames)
    }

    // 必须在 Core 的下一次轮询结算成功前发现不可投影的终态。
    pub(in crate::openai) fn validate_batch(&self) -> Result<(), ProtocolErrorBody> {
        if self.canonical_completed && !self.is_completed() && !self.wire_failure {
            return Err(invalid_response());
        }
        Ok(())
    }

    fn text_snapshot(
        &mut self,
        key: (u64, u64),
        value: &str,
        refusal: bool,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        let content = if refusal {
            &mut self.refusal
        } else {
            &mut self.text
        };
        let accumulated = content.entry(key).or_default();
        let suffix = value
            .strip_prefix(accumulated.as_str())
            .ok_or_else(invalid_response)?
            .to_owned();
        value.clone_into(accumulated);
        if !suffix.is_empty() {
            frames.push(self.delta(json!({if refusal { "refusal" } else { "content" }:suffix})));
        }
        Ok(())
    }

    fn reasoning_snapshot(
        &mut self,
        key: (u64, u64),
        value: &str,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        let accumulated = self.reasoning.entry(key).or_default();
        let suffix = value
            .strip_prefix(accumulated.as_str())
            .ok_or_else(invalid_response)?
            .to_owned();
        value.clone_into(accumulated);
        if !suffix.is_empty() {
            frames.push(self.delta(json!({"reasoning_content":suffix})));
        }
        Ok(())
    }

    fn part(
        &mut self,
        key: (u64, u64),
        part: &Value,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        match part.get("type").and_then(Value::as_str) {
            Some("output_text") => {
                self.text_snapshot(key, required_str(part, "text")?, false, frames)
            }
            Some("refusal") => {
                self.text_snapshot(key, required_str(part, "refusal")?, true, frames)
            }
            _ => Err(invalid_response()),
        }
    }

    fn item(
        &mut self,
        index: u64,
        item: &Value,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        // 终态 output 可能只包含部分条目；先按稳定身份关联原始 output_index。
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let known_call = call_id.and_then(|id| {
            self.tools
                .iter()
                .find_map(|(index, tool)| (tool.id == id).then_some(*index))
        });
        let index = item_id
            .and_then(|id| self.item_ids.get(id).copied())
            .or(known_call)
            .unwrap_or(index);
        if let Some(id) = item_id {
            self.item_ids.insert(id.to_owned(), index);
        }
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if required_str(item, "role")? != "assistant" {
                    return Err(invalid_response());
                }
                for (part_index, part) in item
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid_response)?
                    .iter()
                    .enumerate()
                {
                    self.part(
                        (
                            index,
                            u64::try_from(part_index).map_err(|_| invalid_response())?,
                        ),
                        part,
                        frames,
                    )?;
                }
            }
            Some("function_call") => {
                let existing = self.tools.get(&index);
                let id = item
                    .get("call_id")
                    .map(|value| value.as_str().ok_or_else(invalid_response))
                    .transpose()?
                    .filter(|value| !value.is_empty())
                    .or_else(|| existing.map(|tool| tool.id.as_str()))
                    .ok_or_else(invalid_response)?
                    .to_owned();
                let name = item
                    .get("name")
                    .map(|value| value.as_str().ok_or_else(invalid_response))
                    .transpose()?
                    .filter(|value| !value.is_empty())
                    .or_else(|| existing.map(|tool| tool.name.as_str()))
                    .ok_or_else(invalid_response)?
                    .to_owned();
                if id.is_empty() || name.is_empty() {
                    return Err(invalid_response());
                }
                let next_index = self.tools.len();
                if let Some(tool) = self.tools.get(&index) {
                    if tool.id != id || tool.name != name {
                        return Err(invalid_response());
                    }
                } else {
                    if self.legacy_functions && next_index > 0 {
                        return Err(invalid_response());
                    }
                    self.tools.insert(
                        index,
                        Tool {
                            index: next_index,
                            id: id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                        },
                    );
                    frames.push(self.tool_delta(next_index, Some(&id), Some(&name), ""));
                }
                if let Some(arguments) = item.get("arguments") {
                    self.arguments_snapshot(
                        index,
                        arguments.as_str().ok_or_else(invalid_response)?,
                        frames,
                    )?;
                }
            }
            Some("reasoning") => {
                if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                    for (summary_index, part) in summary.iter().enumerate() {
                        if part.get("type").and_then(Value::as_str) == Some("summary_text") {
                            self.reasoning_snapshot(
                                (
                                    index,
                                    u64::try_from(summary_index).map_err(|_| invalid_response())?,
                                ),
                                required_str(part, "text")?,
                                frames,
                            )?;
                        }
                    }
                }
            }
            _ => return Err(invalid_response()),
        }
        Ok(())
    }

    fn arguments_snapshot(
        &mut self,
        index: u64,
        value: &str,
        frames: &mut Vec<Bytes>,
    ) -> Result<(), ProtocolErrorBody> {
        let tool = self.tools.get_mut(&index).ok_or_else(invalid_response)?;
        // 不完整的 done/terminal 快照不能清空已经交付的参数。
        if value.is_empty() {
            return Ok(());
        }
        let suffix = value
            .strip_prefix(&tool.arguments)
            .ok_or_else(invalid_response)?
            .to_owned();
        value.clone_into(&mut tool.arguments);
        let tool_index = tool.index;
        if !suffix.is_empty() {
            frames.push(self.tool_delta(tool_index, None, None, &suffix));
        }
        Ok(())
    }

    fn tool_delta(
        &self,
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        arguments: &str,
    ) -> Bytes {
        let mut function = json!({"arguments": arguments});
        if let Some(name) = name {
            function["name"] = json!(name);
        }
        if self.legacy_functions {
            return self.delta(json!({"function_call": function}));
        }
        let mut call = json!({"index": index, "function": function});
        if let Some(id) = id {
            call["id"] = json!(id);
            call["type"] = json!("function");
        }
        self.delta(json!({"tool_calls": [call]}))
    }

    fn envelope(&self, object: &str, choices: Value) -> Value {
        json!({"id":self.id,"object":object,"created":self.created,"model":self.model,"choices":choices})
    }

    fn delta(&self, delta: Value) -> Bytes {
        let mut value = self.envelope(
            "chat.completion.chunk",
            json!([{"index":0,"delta":delta,"finish_reason":null}]),
        );
        if self.include_usage {
            value["usage"] = Value::Null;
        }
        frame(&value)
    }

    pub(in crate::openai) fn completed_frames(&mut self) -> Vec<Bytes> {
        let mut frames = std::mem::take(&mut self.terminal_frames);
        let mut finish = self.envelope(
            "chat.completion.chunk",
            json!([{"index":0,"delta":{},"finish_reason":self.finish_reason}]),
        );
        if self.include_usage {
            finish["usage"] = Value::Null;
        }
        frames.push(frame(&finish));
        if self.include_usage && self.usage.is_some() {
            let mut value = self.envelope("chat.completion.chunk", json!([]));
            value["usage"] = self.usage.clone().unwrap_or(Value::Null);
            frames.push(frame(&value));
        }
        frames
    }

    pub(in crate::openai) fn finish(self) -> Result<Value, ProtocolErrorBody> {
        let finish_reason = self.finish_reason.ok_or_else(invalid_response)?;
        let mut message = json!({"role":"assistant","content":null,"refusal":null});
        if !self.text.is_empty() {
            message["content"] = Value::String(self.text.values().cloned().collect());
        }
        if !self.refusal.is_empty() {
            message["refusal"] = Value::String(self.refusal.values().cloned().collect());
        }
        if !self.reasoning.is_empty() {
            message["reasoning_content"] =
                Value::String(self.reasoning.values().cloned().collect());
        }
        if !self.tools.is_empty() {
            let mut tools: Vec<_> = self.tools.values().collect();
            tools.sort_by_key(|tool| tool.index);
            if self.legacy_functions {
                message["function_call"] =
                    json!({"name":tools[0].name,"arguments":tools[0].arguments});
            } else {
                message["tool_calls"] = Value::Array(tools.iter().map(|tool| json!({"id":tool.id,"type":"function","function":{"name":tool.name,"arguments":tool.arguments}})).collect());
            }
        }
        let mut response = self.envelope(
            "chat.completion",
            json!([{"index":0,"message":message,"finish_reason":finish_reason,"logprobs":null}]),
        );
        if let Some(usage) = self.usage {
            response["usage"] = usage;
        }
        Ok(response)
    }
}

fn chat_usage(usage: &Value) -> Result<Value, ProtocolErrorBody> {
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)?;
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)?;
    let total = input.checked_add(output).ok_or_else(invalid_response)?;
    if let Some(supplied) = usage.get("total_tokens")
        && supplied.as_u64() != Some(total)
    {
        return Err(invalid_response());
    }
    let mut value = json!({"prompt_tokens":input,"completion_tokens":output,"total_tokens":total});
    for (source, target, fields) in [
        (
            "input_tokens_details",
            "prompt_tokens_details",
            &[
                "cached_tokens",
                "cache_write_tokens",
                "audio_tokens",
                "image_tokens",
                "text_tokens",
            ][..],
        ),
        (
            "output_tokens_details",
            "completion_tokens_details",
            &[
                "reasoning_tokens",
                "audio_tokens",
                "text_tokens",
                "accepted_prediction_tokens",
                "rejected_prediction_tokens",
            ][..],
        ),
    ] {
        let details = usage
            .get(source)
            .filter(|value| !value.is_null())
            .map(|value| value.as_object().ok_or_else(invalid_response))
            .transpose()?;
        let aliases = usage
            .get(target)
            .filter(|value| !value.is_null())
            .map(|value| value.as_object().ok_or_else(invalid_response))
            .transpose()?;
        let mut mapped = serde_json::Map::new();
        for field in fields {
            if let Some(count) = details
                .and_then(|details| details.get(*field))
                .filter(|value| !value.is_null())
                .or_else(|| {
                    aliases
                        .and_then(|details| details.get(*field))
                        .filter(|value| !value.is_null())
                })
            {
                let count = count.as_u64().ok_or_else(invalid_response)?;
                mapped.insert((*field).to_owned(), json!(count));
            }
        }
        if !mapped.is_empty() {
            value[target] = Value::Object(mapped);
        }
    }
    for (source, target) in [
        ("cached_tokens", "prompt_tokens_details"),
        ("cache_write_tokens", "prompt_tokens_details"),
        ("reasoning_tokens", "completion_tokens_details"),
    ] {
        if value
            .get(target)
            .and_then(|details| details.get(source))
            .is_none()
            && let Some(count) = usage.get(source)
        {
            let count = count.as_u64().ok_or_else(invalid_response)?;
            if value.get(target).is_none() {
                value[target] = json!({});
            }
            value[target][source] = json!(count);
        }
    }
    Ok(value)
}

fn content_key(value: &Value) -> Result<(u64, u64), ProtocolErrorBody> {
    Ok((
        required_index(value, "output_index")?,
        required_index(value, "content_index")?,
    ))
}

fn required_index(value: &Value, key: &str) -> Result<u64, ProtocolErrorBody> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(invalid_response)
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProtocolErrorBody> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)
}

fn frame(value: &Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

fn invalid_response() -> ProtocolErrorBody {
    ProtocolErrorBody {
        error: ProtocolError {
            kind: "server_error",
            code: "invalid_upstream_response",
            message: "The gateway could not convert the upstream response to Chat Completions."
                .to_owned(),
            param: None,
        },
    }
}
