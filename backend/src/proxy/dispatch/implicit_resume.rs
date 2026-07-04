//! Responses 隐式续接策略。

use std::collections::HashSet;

use serde_json::Value;

use crate::upstream::protocol::responses::CodexResponsesRequest;

/// 隐式续接前的可恢复请求状态。
#[derive(Debug, Clone, PartialEq)]
pub struct ImplicitResumeSnapshot {
    pub input: Vec<Value>,
    pub previous_response_id: Option<String>,
    pub turn_state: Option<String>,
    pub use_websocket: bool,
    pub force_http_sse: bool,
}

impl ImplicitResumeSnapshot {
    /// 捕获当前请求中隐式续接会改写的字段。
    pub fn capture(request: &CodexResponsesRequest) -> Self {
        Self {
            input: request.input().to_vec(),
            previous_response_id: request.previous_response_id().map(ToString::to_string),
            turn_state: request.turn_state.clone(),
            use_websocket: request.use_websocket,
            force_http_sse: request.force_http_sse,
        }
    }

    /// 恢复隐式续接前的请求字段。
    pub fn restore(self, request: &mut CodexResponsesRequest) {
        request.set_input(self.input);
        request.set_previous_response_id(self.previous_response_id);
        request.turn_state = self.turn_state;
        request.use_websocket = self.use_websocket;
        request.force_http_sse = self.force_http_sse;
    }
}

/// 返回续接输入在完整输入中的起始位置。
pub fn continuation_input_start(input: &[Value]) -> usize {
    let mut last_model_output_index = None;
    for (index, item) in input.iter().enumerate() {
        if item.get("role").is_some() {
            if item.get("role").and_then(Value::as_str) == Some("assistant") {
                last_model_output_index = Some(index);
            }
            continue;
        }
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            last_model_output_index = Some(index);
        }
    }
    last_model_output_index.map_or(0, |index| index.saturating_add(1))
}

/// 判断完整输入历史是否可以隐式续接到已记录响应。
pub fn implicit_resume_allowed(
    continuation_input: &[Value],
    full_input: &[Value],
    stored_function_call_ids: &[String],
) -> bool {
    let required_call_ids = function_call_output_ids(continuation_input);
    if required_call_ids.is_empty() {
        return stored_function_call_ids.is_empty();
    }

    let inline_call_ids = inline_function_call_ids(full_input);
    if required_call_ids
        .iter()
        .all(|call_id| inline_call_ids.contains(call_id))
    {
        return false;
    }

    let stored_call_ids = stored_function_call_ids.iter().collect::<HashSet<_>>();
    let required_call_ids = required_call_ids.iter().collect::<HashSet<_>>();
    required_call_ids
        .iter()
        .all(|call_id| stored_call_ids.contains(*call_id))
        && stored_call_ids
            .iter()
            .all(|call_id| required_call_ids.contains(*call_id))
}

fn function_call_output_ids(input: &[Value]) -> Vec<String> {
    input
        .iter()
        .filter(|item| item.get("role").is_none())
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .filter(|call_id| !call_id.trim().is_empty())
        .map(ToString::to_string)
        .collect()
}

fn inline_function_call_ids(input: &[Value]) -> HashSet<String> {
    input
        .iter()
        .filter(|item| item.get("role").is_none())
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .filter(|call_id| !call_id.trim().is_empty())
        .map(ToString::to_string)
        .collect()
}
