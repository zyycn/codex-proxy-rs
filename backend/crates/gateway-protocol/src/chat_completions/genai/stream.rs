//! GenAI 原生候选增量直接产生 Chat chunk，EOF 校验生成终态

use super::response::{annotations, logprobs, usage};
use crate::chat_completions::{
    ChatError, ChatIdentity,
    response::{chunk, custom_tools, tool_call, usage_chunk},
};
use crate::genai::{GenerateContentObservation, GenerationFinish};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub struct ChatStreamEncoder {
    identity: ChatIdentity,
    include_usage: bool,
    custom: BTreeSet<String>,
    observation: GenerateContentObservation,
    usage: Option<Value>,
    started: bool,
    completed: bool,
    failed: bool,
    candidate_finished: bool,
    calls: BTreeSet<String>,
    text: String,
    annotations: Vec<Value>,
    required_search: bool,
    grounded: bool,
}

impl ChatStreamEncoder {
    pub fn new(identity: ChatIdentity, include_usage: bool) -> Self {
        Self {
            identity,
            include_usage,
            custom: BTreeSet::new(),
            observation: GenerateContentObservation::default(),
            usage: None,
            started: false,
            completed: false,
            failed: false,
            candidate_finished: false,
            calls: BTreeSet::new(),
            text: String::new(),
            annotations: Vec::new(),
            required_search: false,
            grounded: false,
        }
    }

    pub fn with_request(
        identity: ChatIdentity,
        include_usage: bool,
        request: &Map<String, Value>,
    ) -> Self {
        Self {
            custom: custom_tools(request),
            required_search: request
                .get("web_search_options")
                .is_some_and(|v| !v.is_null()),
            ..Self::new(identity, include_usage)
        }
    }

    pub fn push_event(
        &mut self,
        _event: Option<&str>,
        value: &Value,
    ) -> Result<Vec<Value>, ChatError> {
        if self.failed {
            return Ok(Vec::new());
        }
        if self.completed || !value.is_object() {
            return Err(ChatError::InvalidResponse);
        }
        if let Some(error) = value.get("error") {
            if !error.is_object() {
                return Err(ChatError::InvalidResponse);
            }
            self.failed = true;
            return Ok(vec![json!({"error":error})]);
        }
        if let Some(usage) = self.observation.observe(value).and_then(usage) {
            self.usage = Some(usage);
        }
        let mut frames = Vec::new();
        if !self.started {
            self.started = true;
            frames.push(chunk(
                &self.identity,
                json!({"role":"assistant","content":"","refusal":null}),
                None,
                self.include_usage,
            ));
        }
        let candidates = match value.get("candidates") {
            None | Some(Value::Null) => &[][..],
            Some(value) => value
                .as_array()
                .map(Vec::as_slice)
                .ok_or(ChatError::InvalidResponse)?,
        };
        if candidates.len() > 1 {
            return Err(ChatError::InvalidResponse);
        }
        for candidate in candidates {
            if candidate
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                != 0
            {
                return Err(ChatError::InvalidResponse);
            }
            let parts = match candidate.pointer("/content/parts") {
                None | Some(Value::Null) => &[][..],
                Some(value) => value
                    .as_array()
                    .map(Vec::as_slice)
                    .ok_or(ChatError::InvalidResponse)?,
            };
            if self.candidate_finished && !parts.is_empty() {
                return Err(ChatError::InvalidResponse);
            }
            let start = self.text.chars().count();
            for part in parts {
                let delta = if let Some(text) = part.get("text") {
                    let text = text.as_str().ok_or(ChatError::InvalidResponse)?;
                    if part["thought"] == true {
                        json!({"reasoning_content":text})
                    } else {
                        self.text.push_str(text);
                        json!({"content":text})
                    }
                } else if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .ok_or(ChatError::InvalidResponse)?;
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
                    let index = self.calls.len();
                    if !self.calls.insert(id.clone()) {
                        return Err(ChatError::InvalidResponse);
                    }
                    let input = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    let mut call = tool_call(&id, name, &input, &self.custom)?;
                    call["index"] = json!(index);
                    json!({"tool_calls":[call]})
                } else if let Some(data) = part.get("inlineData") {
                    let mime = data
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .filter(|v| v.starts_with("image/"))
                        .ok_or(ChatError::InvalidResponse)?;
                    let data = data
                        .get("data")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .ok_or(ChatError::InvalidResponse)?;
                    json!({"images":[{"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}")}}]})
                } else if part.get("thoughtSignature").is_some() {
                    continue;
                } else {
                    return Err(ChatError::InvalidResponse);
                };
                frames.push(chunk(&self.identity, delta, None, self.include_usage));
            }
            let metadata = &candidate["groundingMetadata"];
            self.grounded |= metadata["webSearchQueries"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
                || metadata["groundingChunks"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v["web"]["uri"].is_string()));
            let new_annotations = annotations(candidate, &self.text, start)
                .into_iter()
                .filter(|v| !self.annotations.contains(v))
                .collect::<Vec<_>>();
            if !new_annotations.is_empty() {
                self.annotations.extend(new_annotations.iter().cloned());
                frames.push(chunk(
                    &self.identity,
                    json!({"annotations":new_annotations}),
                    None,
                    self.include_usage,
                ));
            }
            if let Some(logprobs) = logprobs(candidate)? {
                let mut frame = chunk(&self.identity, json!({}), None, self.include_usage);
                frame["choices"][0]["logprobs"] = logprobs;
                frames.push(frame);
            }
            if let Some(reason) = candidate
                .get("finishReason")
                .and_then(Value::as_str)
                .filter(|v| !matches!(*v, "" | "FINISH_REASON_UNSPECIFIED"))
            {
                if self.candidate_finished {
                    return Err(ChatError::InvalidResponse);
                }
                if !matches!(
                    reason,
                    "STOP"
                        | "MAX_TOKENS"
                        | "SAFETY"
                        | "RECITATION"
                        | "BLOCKLIST"
                        | "PROHIBITED_CONTENT"
                        | "SPII"
                        | "IMAGE_SAFETY"
                        | "MALFORMED_FUNCTION_CALL"
                        | "UNEXPECTED_TOOL_CALL"
                        | "OTHER"
                        | "LANGUAGE"
                        | "NO_IMAGE"
                        | "IMAGE_OTHER"
                        | "IMAGE_RECITATION"
                ) {
                    return Err(ChatError::InvalidResponse);
                }
                self.candidate_finished = true;
            }
        }
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<Vec<Value>, ChatError> {
        if self.completed || self.failed {
            return Ok(Vec::new());
        }
        let reason = match self
            .observation
            .finish()
            .ok_or(ChatError::IncompleteStream)?
        {
            GenerationFinish::Blocked => "content_filter",
            GenerationFinish::Candidate {
                reason: "STOP",
                tool_call,
            } => {
                if self.required_search && !self.grounded {
                    return Err(ChatError::InvalidResponse);
                }
                if tool_call { "tool_calls" } else { "stop" }
            }
            GenerationFinish::Candidate {
                reason: "MAX_TOKENS",
                ..
            } => "length",
            GenerationFinish::Candidate {
                reason:
                    "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                    | "IMAGE_SAFETY",
                ..
            } => "content_filter",
            GenerationFinish::Candidate { .. } => return Err(ChatError::InvalidResponse),
        };
        self.completed = true;
        let mut frames = vec![chunk(
            &self.identity,
            json!({}),
            Some(reason),
            self.include_usage,
        )];
        if self.include_usage
            && let Some(usage) = self.usage.take()
        {
            frames.push(usage_chunk(&self.identity, usage));
        }
        Ok(frames)
    }
}
