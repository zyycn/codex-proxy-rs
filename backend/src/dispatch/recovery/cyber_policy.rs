//! `cyber_policy` 的会话级换号恢复。

use std::{collections::BTreeSet, sync::Arc, time::Duration as StdDuration};

use chrono::Duration;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::time::timeout;

use crate::{
    dispatch::affinity::{
        CyberPolicyFailureSnapshot, CyberPolicySessionState, SessionAffinityService,
    },
    upstream::openai::{
        protocol::{
            responses::{CodexResponsesRequest, ResponsesSseFailure},
            sse::{SseEvent, SseEventDecoder},
        },
        transport::{CodexClientError, is_cyber_policy_upstream_error},
    },
};

const MAX_ROTATED_ACCOUNTS: usize = 3;
const MAX_FAILURE_MESSAGE_BYTES: usize = 4 * 1024;
const SESSION_STATE_TTL: Duration = Duration::hours(1);
const STATE_IO_TIMEOUT: StdDuration = StdDuration::from_millis(100);

/// 单次请求使用的 `cyber_policy` 会话路由计划。
pub(in crate::dispatch) struct CyberPolicyRoutingPlan {
    session_key: Option<String>,
    state: CyberPolicySessionState,
}

impl CyberPolicyRoutingPlan {
    pub(in crate::dispatch) fn excluded_account_ids(&self) -> BTreeSet<String> {
        self.state.failed_account_ids.iter().cloned().collect()
    }

    pub(in crate::dispatch) fn exhausted_failure(&self) -> Option<ResponsesSseFailure> {
        if self.state.failed_account_ids.len() < MAX_ROTATED_ACCOUNTS {
            return None;
        }
        self.last_failure()
    }

    pub(in crate::dispatch) fn last_failure(&self) -> Option<ResponsesSseFailure> {
        self.state
            .last_failure
            .as_ref()
            .map(snapshot_to_sse_failure)
    }

    pub(in crate::dispatch) fn last_account_id(&self) -> Option<&str> {
        self.state
            .last_failure
            .as_ref()
            .map(|failure| failure.account_id.as_str())
    }

    fn session_key(&self) -> Option<&str> {
        self.session_key.as_deref()
    }

    fn enabled(&self) -> bool {
        self.session_key.is_some()
    }

    fn has_failures(&self) -> bool {
        !self.state.failed_account_ids.is_empty()
    }
}

/// 识别失败、维护会话状态，并为下一次请求生成账号排除集合。
#[derive(Clone)]
pub(in crate::dispatch) struct CyberPolicyRecovery {
    session_affinity: Arc<SessionAffinityService>,
}

/// 在 live SSE 转发前观察终止事件，保证下一请求看到最新会话状态。
#[derive(Default)]
pub(in crate::dispatch) struct CyberPolicyStreamObserver {
    decoder: SseEventDecoder,
    decoder_stopped: bool,
    state_transition_applied: bool,
}

impl CyberPolicyStreamObserver {
    pub(in crate::dispatch) async fn observe_chunk(
        &mut self,
        recovery: &CyberPolicyRecovery,
        plan: &CyberPolicyRoutingPlan,
        account_id: &str,
        chunk: &[u8],
    ) {
        if self.decoder_stopped || self.state_transition_applied || !plan.enabled() {
            return;
        }
        match self.decoder.push(chunk) {
            Ok(events) => {
                self.observe_events(recovery, plan, account_id, &events)
                    .await
            }
            Err(error) => {
                self.decoder_stopped = true;
                tracing::warn!(error = %error, "Failed to decode live SSE for cyber policy state");
            }
        }
    }

    pub(in crate::dispatch) async fn finish(
        &mut self,
        recovery: &CyberPolicyRecovery,
        plan: &CyberPolicyRoutingPlan,
        account_id: &str,
    ) {
        if self.decoder_stopped || self.state_transition_applied || !plan.enabled() {
            return;
        }
        match self.decoder.finish() {
            Ok(events) => {
                self.observe_events(recovery, plan, account_id, &events)
                    .await
            }
            Err(error) => {
                tracing::warn!(error = %error, "Failed to finish live SSE cyber policy state");
            }
        }
    }

    pub(in crate::dispatch) fn state_transition_applied(&self) -> bool {
        self.state_transition_applied
    }

    async fn observe_events(
        &mut self,
        recovery: &CyberPolicyRecovery,
        plan: &CyberPolicyRoutingPlan,
        account_id: &str,
        events: &[SseEvent],
    ) {
        let mut completed = false;
        for event in events {
            match event.event.as_deref() {
                Some("response.completed" | "response.incomplete") => {
                    completed |= serde_json::from_str::<Value>(&event.data)
                        .is_ok_and(|value| value.get("response").is_some_and(Value::is_object));
                    continue;
                }
                Some(event_name @ ("error" | "response.failed")) => {
                    let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                        continue;
                    };
                    let failure = ResponsesSseFailure::from_event(event_name, &value);
                    if is_cyber_policy_failure(&failure) {
                        recovery
                            .observe_sse_failure(plan, account_id, &failure)
                            .await;
                        self.state_transition_applied = true;
                        return;
                    }
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
            let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                continue;
            };
            match value.get("type").and_then(Value::as_str) {
                Some(event_name @ ("error" | "response.failed")) => {
                    let failure = ResponsesSseFailure::from_event(event_name, &value);
                    if is_cyber_policy_failure(&failure) {
                        recovery
                            .observe_sse_failure(plan, account_id, &failure)
                            .await;
                        self.state_transition_applied = true;
                        return;
                    }
                }
                Some("response.completed" | "response.incomplete") => {
                    completed |= value.get("response").is_some_and(Value::is_object);
                }
                _ => {}
            }
        }
        if completed {
            recovery.observe_success(plan).await;
            self.state_transition_applied = true;
        }
    }
}

impl CyberPolicyRecovery {
    pub(in crate::dispatch) fn new(session_affinity: Arc<SessionAffinityService>) -> Self {
        Self { session_affinity }
    }

    pub(in crate::dispatch) async fn prepare(
        &self,
        request: &CodexResponsesRequest,
    ) -> CyberPolicyRoutingPlan {
        let Some(session_key) = session_key(request) else {
            return CyberPolicyRoutingPlan {
                session_key: None,
                state: CyberPolicySessionState::default(),
            };
        };
        let state = match timeout(
            STATE_IO_TIMEOUT,
            self.session_affinity.load_cyber_policy_state(&session_key),
        )
        .await
        {
            Ok(Ok(state)) => state.unwrap_or_default(),
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "Failed to read cyber policy session state");
                CyberPolicySessionState::default()
            }
            Err(_) => {
                tracing::warn!("Timed out reading cyber policy session state");
                CyberPolicySessionState::default()
            }
        };
        CyberPolicyRoutingPlan {
            session_key: Some(session_key),
            state,
        }
    }

    pub(in crate::dispatch) async fn observe_sse_failure(
        &self,
        plan: &CyberPolicyRoutingPlan,
        account_id: &str,
        failure: &ResponsesSseFailure,
    ) {
        if !is_cyber_policy_failure(failure) {
            return;
        }
        let Some(session_key) = plan.session_key() else {
            return;
        };
        let snapshot = CyberPolicyFailureSnapshot {
            account_id: account_id.to_string(),
            event: failure.event.clone(),
            message: truncate_utf8(&failure.message, MAX_FAILURE_MESSAGE_BYTES),
            upstream_code: failure.upstream_code.clone(),
        };
        match timeout(
            STATE_IO_TIMEOUT,
            self.session_affinity.persist_cyber_policy_failure(
                session_key,
                &snapshot,
                MAX_ROTATED_ACCOUNTS,
                SESSION_STATE_TTL,
            ),
        )
        .await
        {
            Ok(Ok(state)) => tracing::warn!(
                account_id,
                failed_account_count = state.failed_account_ids.len(),
                "Recorded cyber policy failure for the next request in this session"
            ),
            Ok(Err(error)) => tracing::warn!(
                account_id,
                error = %error,
                "Failed to record cyber policy session state"
            ),
            Err(_) => tracing::warn!(account_id, "Timed out recording cyber policy session state"),
        }
    }

    pub(in crate::dispatch) async fn observe_upstream_error(
        &self,
        plan: &CyberPolicyRoutingPlan,
        account_id: &str,
        error: &CodexClientError,
    ) {
        let Some(failure) = failure_from_upstream_error(error) else {
            return;
        };
        self.observe_sse_failure(plan, account_id, &failure).await;
    }

    pub(in crate::dispatch) async fn observe_success(&self, plan: &CyberPolicyRoutingPlan) {
        if !plan.has_failures() {
            return;
        }
        let Some(session_key) = plan.session_key() else {
            return;
        };
        match timeout(
            STATE_IO_TIMEOUT,
            self.session_affinity
                .delete_cyber_policy_state(session_key, &plan.state.revision),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "Failed to clear cyber policy session state");
            }
            Err(_) => tracing::warn!("Timed out clearing cyber policy session state"),
        }
    }
}

fn session_key(request: &CodexResponsesRequest) -> Option<String> {
    if request.previous_response_id().is_some() {
        return None;
    }
    let api_key_id = non_empty(request.client_api_key_id.as_deref())?;
    let explicit_session_id = non_empty(request.client_session_id.as_deref())
        .or_else(|| non_empty(request.client_conversation_id.as_deref()))
        .or_else(|| {
            request
                .explicit_prompt_cache_key
                .then(|| non_empty(request.prompt_cache_key()))
                .flatten()
        })?;
    let mut hasher = Sha256::new();
    hasher.update(b"cyber-policy-session\0");
    hasher.update(api_key_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(explicit_session_id.as_bytes());
    Some(hex::encode(hasher.finalize()))
}

fn failure_from_upstream_error(error: &CodexClientError) -> Option<ResponsesSseFailure> {
    let CodexClientError::Upstream { body, .. } = error else {
        return None;
    };
    if !is_cyber_policy_upstream_error(error) {
        return None;
    }
    let value = serde_json::from_str::<Value>(body).ok()?;
    let code = value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)?;
    let message = value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("Codex upstream SSE failed")
        .to_string();
    Some(ResponsesSseFailure {
        event: "error".to_string(),
        message,
        upstream_code: Some(code.to_string()),
    })
}

fn is_cyber_policy_code(code: Option<&str>) -> bool {
    code.is_some_and(|code| code.trim().eq_ignore_ascii_case("cyber_policy"))
}

pub(crate) fn is_cyber_policy_failure(failure: &ResponsesSseFailure) -> bool {
    is_cyber_policy_code(failure.upstream_code.as_deref())
}

fn snapshot_to_sse_failure(snapshot: &CyberPolicyFailureSnapshot) -> ResponsesSseFailure {
    ResponsesSseFailure {
        event: snapshot.event.clone(),
        message: snapshot.message.clone(),
        upstream_code: snapshot.upstream_code.clone(),
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
