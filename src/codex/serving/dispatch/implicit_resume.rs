use std::collections::HashSet;

use serde_json::Value;

use crate::codex::gateway::transport::types::CodexResponsesRequest;

#[derive(Debug, Clone)]
pub(crate) struct ImplicitResumeSnapshot {
    input: Vec<Value>,
    previous_response_id: Option<String>,
    turn_state: Option<String>,
    use_websocket: bool,
    force_http_sse: bool,
}

impl ImplicitResumeSnapshot {
    pub(super) fn capture(request: &CodexResponsesRequest) -> Self {
        Self {
            input: request.input.clone(),
            previous_response_id: request.previous_response_id.clone(),
            turn_state: request.turn_state.clone(),
            use_websocket: request.use_websocket,
            force_http_sse: request.force_http_sse,
        }
    }

    pub(crate) fn restore(self, request: &mut CodexResponsesRequest) {
        request.input = self.input;
        request.previous_response_id = self.previous_response_id;
        request.turn_state = self.turn_state;
        request.use_websocket = self.use_websocket;
        request.force_http_sse = self.force_http_sse;
    }
}

pub(super) fn continuation_input_start(input: &[Value]) -> usize {
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

pub(super) fn implicit_resume_allowed(
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
