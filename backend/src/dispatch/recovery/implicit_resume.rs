//! Responses 隐式续接策略。

use std::collections::HashSet;

use chrono::{Duration, Utc};
use serde_json::Value;

use crate::{
    dispatch::{
        affinity::{
            compute_variant_hash, ensure_prompt_cache_key, hash_instructions,
            prepare_variant_identity,
        },
        service::ResponseDispatchService,
    },
    upstream::openai::protocol::responses::CodexResponsesRequest,
};

const IMPLICIT_RESUME_MAX_AGE_SECS: i64 = 55 * 60;

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

impl ResponseDispatchService {
    pub(in crate::dispatch) async fn prepare_response_session(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        prepare_variant_identity(request);
        if let Some(previous_response_id) = request.previous_response_id().map(ToString::to_string)
        {
            if request.prompt_cache_key().is_none() {
                let conversation_id = self
                    .session_affinity
                    .lookup_conversation_id(&previous_response_id, Utc::now())
                    .await;
                request.set_prompt_cache_key(conversation_id);
            }
            if request.turn_state.as_deref().is_none_or(str::is_empty) {
                request.turn_state = self
                    .session_affinity
                    .lookup_turn_state(&previous_response_id, Utc::now())
                    .await;
            }
            ensure_prompt_cache_key(request);
            return None;
        }

        ensure_prompt_cache_key(request);
        self.apply_implicit_resume(request).await
    }

    async fn apply_implicit_resume(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        let continuation_start = continuation_input_start(request.input());
        if continuation_start == 0 || continuation_start >= request.input().len() {
            return None;
        }
        let conversation_id = request
            .prompt_cache_key()
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let snapshot = ImplicitResumeSnapshot::capture(request);
        let variant_hash = compute_variant_hash(request);
        let now = Utc::now();
        let previous_response_id = self
            .session_affinity
            .lookup_latest_response_by_conversation(
                &conversation_id,
                Some(Duration::seconds(IMPLICIT_RESUME_MAX_AGE_SECS)),
                Some(&variant_hash),
                now,
            )
            .await?;
        let current_instructions_hash = hash_instructions(Some(request.instructions()));
        if self
            .session_affinity
            .lookup_instructions_hash(&previous_response_id, now)
            .await
            .as_deref()
            != Some(current_instructions_hash.as_str())
        {
            return None;
        }
        let stored_function_call_ids = self
            .session_affinity
            .lookup_function_call_ids(&previous_response_id, now)
            .await;
        if !implicit_resume_allowed(
            &request.input()[continuation_start..],
            request.input(),
            &stored_function_call_ids,
        ) {
            return None;
        }
        let account_id = self
            .session_affinity
            .lookup_account(&previous_response_id, now)
            .await?;
        let replay_items = self.reasoning_replay.lock().await.lookup(
            &previous_response_id,
            &account_id,
            &conversation_id,
            &variant_hash,
            now,
        );
        let continuation = request.input()[continuation_start..].to_vec();
        let mut input = replay_items;
        input.extend(continuation);

        request.set_previous_response_id(Some(previous_response_id.clone()));
        request.use_websocket = true;
        request.force_http_sse = false;
        request.set_input(input);
        if let Some(turn_state) = self
            .session_affinity
            .lookup_turn_state(&previous_response_id, now)
            .await
        {
            request.turn_state = Some(turn_state);
        }

        Some(snapshot)
    }

    pub(in crate::dispatch) async fn try_recover_implicit_resume(
        &self,
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
        account_id: &str,
        evict_reasoning_replay: bool,
    ) -> bool {
        let Some(snapshot) = implicit_resume.take() else {
            return false;
        };
        if evict_reasoning_replay {
            self.evict_reasoning_replay(request, account_id).await;
        }
        if let Some(previous_response_id) = request.previous_response_id() {
            self.session_affinity.forget(previous_response_id).await;
        }
        restore_full_request_without_history(request, snapshot);
        true
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

fn strip_request_history(request: &mut CodexResponsesRequest) {
    request.set_previous_response_id(None);
    request.turn_state = None;
}

pub(in crate::dispatch) fn restore_full_request_without_history(
    request: &mut CodexResponsesRequest,
    snapshot: ImplicitResumeSnapshot,
) {
    snapshot.restore(request);
    strip_request_history(request);
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
