//! GenAI 完整响应、用量和来源直接投影为 Chat

use super::stream::ChatStreamEncoder;
use crate::chat_completions::{ChatAccumulator, ChatError, ChatIdentity};
use crate::genai::UsageMetadata;
use serde_json::{Map, Value, json};

pub fn project_response(response: &Value, identity: &ChatIdentity) -> Result<Value, ChatError> {
    project(response, ChatStreamEncoder::new(identity.clone(), true))
}

pub fn project_response_with_request(
    response: &Value,
    identity: &ChatIdentity,
    request: &Map<String, Value>,
) -> Result<Value, ChatError> {
    project(
        response,
        ChatStreamEncoder::with_request(identity.clone(), true, request),
    )
}

fn project(response: &Value, mut encoder: ChatStreamEncoder) -> Result<Value, ChatError> {
    let mut accumulator = ChatAccumulator::default();
    for chunk in encoder.push_event(None, response)? {
        accumulator.push(&chunk)?;
    }
    for chunk in encoder.finish()? {
        accumulator.push(&chunk)?;
    }
    accumulator.finish()
}

pub(super) fn usage(usage: &UsageMetadata) -> Option<Value> {
    let input = usage.input_tokens()?;
    let output = usage.output_tokens()?;
    let mut result = json!({"prompt_tokens":input,"completion_tokens":output,"total_tokens":usage.total.unwrap_or_else(||input.saturating_add(output))});
    if let Some(cached) = usage.cached {
        result["prompt_tokens_details"]["cached_tokens"] = json!(cached);
    }
    if let Some(image) = usage.image_input_tokens() {
        result["prompt_tokens_details"]["image_tokens"] = json!(image);
    }
    if let Some(reasoning) = usage.thoughts {
        result["completion_tokens_details"]["reasoning_tokens"] = json!(reasoning);
    }
    if let Some(image) = usage.candidate_image {
        result["completion_tokens_details"]["image_tokens"] = json!(image);
    }
    Some(result)
}

pub(super) fn annotations(candidate: &Value, full_text: &str, text_start: usize) -> Vec<Value> {
    let mut positions = Vec::new();
    let mut char_offset = text_start;
    for part in candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let text = part
            .get("text")
            .and_then(Value::as_str)
            .filter(|_| part["thought"] != true);
        positions.push(text.map(|text| {
            let offset = char_offset;
            char_offset += text.chars().count();
            (offset, text)
        }));
    }
    if positions.is_empty() {
        positions.push(Some((0, full_text)));
    }
    let metadata = &candidate["groundingMetadata"];
    let Some(chunks) = metadata["groundingChunks"].as_array() else {
        return Vec::new();
    };
    let mut output = Vec::new();
    for support in metadata["groundingSupports"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let segment = &support["segment"];
        let index = segment
            .get("partIndex")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize;
        let Some(Some((offset, text))) = positions.get(index) else {
            continue;
        };
        let start = segment
            .get("startIndex")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize;
        let Some(end) = segment
            .get("endIndex")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
        else {
            continue;
        };
        if start > end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        if let Some(expected) = segment.get("text").and_then(Value::as_str)
            && text.get(start..end) != Some(expected)
        {
            continue;
        }
        let start = offset + text[..start].chars().count();
        let end = offset + text[..end].chars().count();
        for index in support["groundingChunkIndices"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
        {
            let Some(source) = chunks.get(index as usize).and_then(|v| v.get("web")) else {
                continue;
            };
            let Some(url) = source.get("uri").and_then(Value::as_str) else {
                continue;
            };
            if !url::Url::parse(url)
                .is_ok_and(|v| matches!(v.scheme(), "http" | "https") && v.host_str().is_some())
            {
                continue;
            }
            let annotation = json!({"type":"url_citation","url_citation":{"url":url,"title":source.get("title").and_then(Value::as_str).unwrap_or(url),"start_index":start,"end_index":end}});
            if !output.contains(&annotation) {
                output.push(annotation);
            }
        }
    }
    output
}

pub(super) fn logprobs(candidate: &Value) -> Result<Option<Value>, ChatError> {
    let Some(result) = candidate.get("logprobsResult").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let chosen = result
        .get("chosenCandidates")
        .and_then(Value::as_array)
        .ok_or(ChatError::InvalidResponse)?;
    let top = result.get("topCandidates").and_then(Value::as_array);
    let tokens = chosen
        .iter()
        .enumerate()
        .map(|(index, chosen)| {
            let mut token = logprob(chosen)?;
            let top = top
                .and_then(|v| v.get(index))
                .and_then(|v| v.get("candidates"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(logprob)
                .collect::<Result<Vec<_>, _>>()?;
            token["top_logprobs"] = top.into();
            Ok(token)
        })
        .collect::<Result<Vec<_>, ChatError>>()?;
    Ok(Some(json!({"content":tokens,"refusal":null})))
}

fn logprob(value: &Value) -> Result<Value, ChatError> {
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .ok_or(ChatError::InvalidResponse)?;
    let probability = value
        .get("logProbability")
        .filter(|v| v.is_number())
        .ok_or(ChatError::InvalidResponse)?;
    Ok(json!({"token":token,"logprob":probability,"bytes":token.as_bytes()}))
}
