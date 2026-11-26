//! Chat 响应封套及直接 Chat 流的完整结果聚合

use super::ChatError;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// 响应身份由请求执行边界提供，转换器不创建执行状态
#[derive(Debug, Clone, Serialize)]
pub struct ChatIdentity {
    pub id: String,
    pub created: u64,
    pub model: String,
}

pub(super) fn completion(
    identity: &ChatIdentity,
    message: Value,
    reason: &str,
    usage: Option<Value>,
) -> Value {
    let mut result = json!({"id":identity.id,"object":"chat.completion","created":identity.created,"model":identity.model,"choices":[{"index":0,"message":message,"finish_reason":reason,"logprobs":null}]});
    if let Some(usage) = usage {
        result["usage"] = usage;
    }
    result
}

pub(super) fn chunk(
    identity: &ChatIdentity,
    delta: Value,
    reason: Option<&str>,
    include_usage: bool,
) -> Value {
    let mut result = json!({"id":identity.id,"object":"chat.completion.chunk","created":identity.created,"model":identity.model,"choices":[{"index":0,"delta":delta,"finish_reason":reason,"logprobs":null}]});
    if include_usage {
        result["usage"] = Value::Null;
    }
    result
}

pub(super) fn usage_chunk(identity: &ChatIdentity, usage: Value) -> Value {
    json!({"id":identity.id,"object":"chat.completion.chunk","created":identity.created,"model":identity.model,"choices":[],"usage":usage})
}

/// 完整 Chat 结果直接交付为 Chat chunk，不借用其他协议的事件生命周期
pub fn completion_chunks(completion: &Value, include_usage: bool) -> Result<Vec<Value>, ChatError> {
    let mut envelope = completion
        .as_object()
        .cloned()
        .ok_or(ChatError::InvalidResponse)?;
    if envelope.get("object").and_then(Value::as_str) != Some("chat.completion")
        || !envelope.get("id").is_some_and(Value::is_string)
        || !envelope.get("model").is_some_and(Value::is_string)
        || !envelope.get("created").is_some_and(Value::is_u64)
    {
        return Err(ChatError::InvalidResponse);
    }
    let choices = envelope
        .remove("choices")
        .ok_or(ChatError::InvalidResponse)?;
    let choices = choices
        .as_array()
        .filter(|choices| !choices.is_empty())
        .ok_or(ChatError::InvalidResponse)?;
    let usage = envelope.remove("usage").filter(|value| !value.is_null());
    envelope.insert("object".into(), json!("chat.completion.chunk"));
    if include_usage {
        envelope.insert("usage".into(), Value::Null);
    }
    let mut roles = Vec::with_capacity(choices.len());
    let mut contents = Vec::with_capacity(choices.len());
    let mut terminals = Vec::with_capacity(choices.len());
    let mut indices = BTreeSet::new();
    for choice in choices {
        let index = choice
            .get("index")
            .and_then(Value::as_u64)
            .ok_or(ChatError::InvalidResponse)?;
        if !indices.insert(index) {
            return Err(ChatError::InvalidResponse);
        }
        let mut message = choice
            .get("message")
            .and_then(Value::as_object)
            .cloned()
            .ok_or(ChatError::InvalidResponse)?;
        let role = message
            .remove("role")
            .filter(|role| role == "assistant")
            .ok_or(ChatError::InvalidResponse)?;
        let reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .ok_or(ChatError::InvalidResponse)?;
        if let Some(calls) = message
            .get_mut("tool_calls")
            .filter(|value| !value.is_null())
        {
            for (index, call) in calls
                .as_array_mut()
                .ok_or(ChatError::InvalidResponse)?
                .iter_mut()
                .enumerate()
            {
                call.as_object_mut()
                    .ok_or(ChatError::InvalidResponse)?
                    .insert("index".into(), json!(index));
            }
        }
        roles.push(
            json!({"index":index,"delta":{"role":role},"finish_reason":null,"logprobs":null}),
        );
        contents.push(json!({"index":index,"delta":message,"finish_reason":null,"logprobs":choice.get("logprobs")}));
        terminals.push(json!({"index":index,"delta":{},"finish_reason":reason,"logprobs":null}));
    }
    let mut chunks = Vec::with_capacity(4);
    for choices in [roles, contents, terminals] {
        let mut chunk = envelope.clone();
        chunk.insert("choices".into(), choices.into());
        chunks.push(Value::Object(chunk));
    }
    if include_usage && let Some(usage) = usage {
        envelope.insert("choices".into(), json!([]));
        envelope.insert("usage".into(), usage);
        chunks.push(Value::Object(envelope));
    }
    Ok(chunks)
}

/// 仅在非流式交付时聚合 Chat chunk，不消费其他平台协议
#[derive(Default)]
pub struct ChatAccumulator {
    envelope: Option<Map<String, Value>>,
    message: Map<String, Value>,
    tools: BTreeMap<u64, Value>,
    finish_reason: Option<String>,
    logprobs: Map<String, Value>,
    error: Option<Value>,
}

impl ChatAccumulator {
    /// 聚合一个 Chat JSON chunk；结束原因后允许独立用量帧
    pub fn push(&mut self, chunk: &Value) -> Result<(), ChatError> {
        if let Some(error) = chunk.get("error") {
            if self.error.is_some() || self.finish_reason.is_some() {
                return Err(ChatError::InvalidResponse);
            }
            self.error = Some(json!({"error":error}));
            return Ok(());
        }
        if self.error.is_some() {
            return Err(ChatError::InvalidResponse);
        }
        let body = chunk.as_object().ok_or(ChatError::InvalidResponse)?;
        if self.envelope.is_none() {
            for field in ["id", "model"] {
                if !body.get(field).is_some_and(Value::is_string) {
                    return Err(ChatError::InvalidResponse);
                }
            }
            if !body.get("created").is_some_and(Value::is_u64) {
                return Err(ChatError::InvalidResponse);
            }
            self.envelope = Some(Map::from_iter([
                ("id".into(), body["id"].clone()),
                ("model".into(), body["model"].clone()),
                ("created".into(), body["created"].clone()),
                ("object".into(), json!("chat.completion")),
            ]));
            self.message.insert("role".into(), json!("assistant"));
            self.message.insert("content".into(), Value::Null);
            self.message.insert("refusal".into(), Value::Null);
        }
        let envelope = self.envelope.as_mut().ok_or(ChatError::InvalidResponse)?;
        for field in ["id", "model", "created"] {
            if body.get(field) != envelope.get(field) {
                return Err(ChatError::InvalidResponse);
            }
        }
        for field in [
            "usage",
            "metadata",
            "service_tier",
            "moderation",
            "system_fingerprint",
        ] {
            if let Some(value) = body.get(field).filter(|v| !v.is_null()) {
                envelope.insert(field.into(), value.clone());
            }
        }
        for choice in body
            .get("choices")
            .and_then(Value::as_array)
            .ok_or(ChatError::InvalidResponse)?
        {
            if choice.get("index").and_then(Value::as_u64) != Some(0)
                || self.finish_reason.is_some()
            {
                return Err(ChatError::InvalidResponse);
            }
            let delta = choice
                .get("delta")
                .and_then(Value::as_object)
                .ok_or(ChatError::InvalidResponse)?;
            for (field, value) in delta {
                if value.is_null() {
                    continue;
                }
                match field.as_str() {
                    "role" if value == "assistant" => {}
                    "content" | "refusal" | "reasoning_content" => {
                        append_string(&mut self.message, field, value)?
                    }
                    "tool_calls" => {
                        for call in value.as_array().ok_or(ChatError::InvalidResponse)? {
                            let index = call
                                .get("index")
                                .and_then(Value::as_u64)
                                .ok_or(ChatError::InvalidResponse)?;
                            let target = self.tools.entry(index).or_insert_with(|| json!({}));
                            for field in ["id", "type"] {
                                if let Some(value) = call.get(field) {
                                    if target.get(field).is_some_and(|v| v != value) {
                                        return Err(ChatError::InvalidResponse);
                                    }
                                    target[field] = value.clone();
                                }
                            }
                            for kind in ["function", "custom"] {
                                if let Some(function) = call.get(kind).and_then(Value::as_object) {
                                    if target.get(kind).is_none() {
                                        target[kind] = json!({});
                                    }
                                    let function_target = target[kind]
                                        .as_object_mut()
                                        .ok_or(ChatError::InvalidResponse)?;
                                    for (key, value) in function {
                                        append_string(function_target, key, value)?;
                                    }
                                }
                            }
                        }
                    }
                    "annotations" => append_array(&mut self.message, field, value)?,
                    "images" => merge_images(&mut self.message, value)?,
                    _ => {
                        self.message.insert(field.clone(), value.clone());
                    }
                }
            }
            if let Some(logprobs) = choice.get("logprobs").and_then(Value::as_object) {
                for (field, value) in logprobs {
                    if !value.is_null() {
                        append_array(&mut self.logprobs, field, value)?;
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                self.finish_reason = Some(
                    reason
                        .as_str()
                        .ok_or(ChatError::InvalidResponse)?
                        .to_owned(),
                );
            }
        }
        Ok(())
    }

    /// 生成完整 Chat 响应；EOF 本身不能补造成功终态
    pub fn finish(mut self) -> Result<Value, ChatError> {
        if let Some(error) = self.error {
            return Ok(error);
        }
        let reason = self.finish_reason.ok_or(ChatError::IncompleteStream)?;
        let mut result = self.envelope.take().ok_or(ChatError::IncompleteStream)?;
        if !self.tools.is_empty() {
            for call in self.tools.values() {
                let kind = call
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or(ChatError::InvalidResponse)?;
                if !matches!(kind, "function" | "custom")
                    || !call.get("id").is_some_and(Value::is_string)
                    || !call
                        .get(kind)
                        .and_then(|v| v.get("name"))
                        .is_some_and(Value::is_string)
                {
                    return Err(ChatError::InvalidResponse);
                }
            }
            self.message.insert(
                "tool_calls".into(),
                self.tools.into_values().collect::<Vec<_>>().into(),
            );
        }
        if let Some(images) = self.message.get_mut("images").and_then(Value::as_array_mut) {
            for image in images {
                if let Some(image) = image.as_object_mut() {
                    image.remove("index");
                }
            }
        }
        for field in ["content", "refusal"] {
            if self.message.get(field).and_then(Value::as_str) == Some("") {
                self.message.insert(field.into(), Value::Null);
            }
        }
        result.insert("choices".into(), json!([{"index":0,"message":self.message,"finish_reason":reason,"logprobs":if self.logprobs.is_empty(){Value::Null}else{Value::Object(self.logprobs)}}]));
        Ok(result.into())
    }
}

// 同一图片的预览和最终图共享 index，完整JSON只保留最后一次快照
fn merge_images(target: &mut Map<String, Value>, value: &Value) -> Result<(), ChatError> {
    let images = value.as_array().ok_or(ChatError::InvalidResponse)?;
    let target = target
        .entry("images")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or(ChatError::InvalidResponse)?;
    for image in images {
        if !image.is_object() {
            return Err(ChatError::InvalidResponse);
        }
        let index = image.get("index").and_then(Value::as_u64);
        if let Some(previous) = index.and_then(|index| {
            target
                .iter_mut()
                .find(|entry| entry.get("index").and_then(Value::as_u64) == Some(index))
        }) {
            *previous = image.clone();
        } else {
            target.push(image.clone());
        }
    }
    Ok(())
}

fn append_string(
    target: &mut Map<String, Value>,
    field: &str,
    value: &Value,
) -> Result<(), ChatError> {
    let fragment = value.as_str().ok_or(ChatError::InvalidResponse)?;
    match target.get_mut(field) {
        Some(Value::String(text)) => text.push_str(fragment),
        None | Some(Value::Null) => {
            target.insert(field.into(), json!(fragment));
        }
        _ => return Err(ChatError::InvalidResponse),
    }
    Ok(())
}

fn append_array(
    target: &mut Map<String, Value>,
    field: &str,
    value: &Value,
) -> Result<(), ChatError> {
    let values = value.as_array().ok_or(ChatError::InvalidResponse)?;
    let target = target
        .entry(field)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or(ChatError::InvalidResponse)?;
    target.extend(values.iter().cloned());
    Ok(())
}

pub(super) fn custom_tools(request: &Map<String, Value>) -> BTreeSet<String> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|v| v["type"] == "custom")
        .filter_map(|v| {
            v.pointer("/custom/name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

pub(super) fn tool_call(
    id: &str,
    name: &str,
    input: &Value,
    custom: &BTreeSet<String>,
) -> Result<Value, ChatError> {
    if !input.is_object() {
        return Err(ChatError::InvalidResponse);
    }
    if custom.contains(name) {
        let input = input
            .get("input")
            .and_then(Value::as_str)
            .ok_or(ChatError::InvalidResponse)?;
        Ok(json!({"id":id,"type":"custom","custom":{"name":name,"input":input}}))
    } else {
        Ok(
            json!({"id":id,"type":"function","function":{"name":name,"arguments":input.to_string()}}),
        )
    }
}
