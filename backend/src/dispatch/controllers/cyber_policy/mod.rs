//! `cyber_policy` 会话级生命周期控制器。

use std::{collections::BTreeSet, sync::Arc, time::Duration as StdDuration};

use chrono::Duration;
use sha2::{Digest, Sha256};
use tokio::time::timeout;

mod store;
mod types;

use crate::{
    dispatch::{
        affinity::SessionAffinityService,
        controllers::ControllerFailureFact,
        errors::ClientFailure,
        lifecycle::contract::{AttemptDecision, AttemptObservation, AttemptReturnKind},
    },
    upstream::openai::{
        failure::UpstreamFailureFacts,
        protocol::responses::{CodexResponsesRequest, ResponsesSseFailure},
    },
};

use self::{store::Store, types::SessionState};

const SESSION_STATE_TTL: Duration = Duration::hours(1);
const STATE_IO_TIMEOUT: StdDuration = StdDuration::from_millis(100);

/// 单次请求使用的 `cyber_policy` 会话路由计划。
pub(in crate::dispatch) struct CyberPolicyScope {
    session_key: Option<String>,
    state: SessionState,
}

impl CyberPolicyScope {
    pub(in crate::dispatch) fn excluded_account_ids(&self) -> BTreeSet<String> {
        self.state.failed_account_ids.iter().cloned().collect()
    }

    fn session_key(&self) -> Option<&str> {
        self.session_key.as_deref()
    }

    fn has_failures(&self) -> bool {
        !self.state.failed_account_ids.is_empty()
    }
}

/// 识别失败、维护会话状态，并为下一次请求生成账号排除集合。
#[derive(Clone)]
pub(in crate::dispatch) struct CyberPolicyController {
    store: Store,
}

/// 已由 Cyber owner 认领的失败；调用方只能请求 owner 产生 effect 或 decision。
pub(super) struct ClassifiedCyberPolicyFailure<'a> {
    fact: ControllerFailureFact<'a>,
}

impl CyberPolicyController {
    pub(in crate::dispatch) fn new(session_affinity: Arc<SessionAffinityService>) -> Self {
        Self {
            store: Store::new(session_affinity.redis_connection()),
        }
    }

    pub(super) fn classify(
        fact: ControllerFailureFact<'_>,
    ) -> Option<ClassifiedCyberPolicyFailure<'_>> {
        let owned = match fact {
            ControllerFailureFact::Upstream(facts) => {
                facts
                    .status_code
                    .is_some_and(|status| (400..=499).contains(&status))
                    && is_cyber_policy_code(facts.code.as_deref())
            }
            ControllerFailureFact::Response(failure) => is_cyber_policy_failure(failure),
        };
        owned.then_some(ClassifiedCyberPolicyFailure { fact })
    }

    pub(super) fn client_failure(classified: &ClassifiedCyberPolicyFailure<'_>) -> ClientFailure {
        match classified.fact {
            ControllerFailureFact::Upstream(facts) => ClientFailure::new(
                failure_from_upstream_facts(facts),
                facts.status_code.unwrap_or(400),
                true,
            ),
            ControllerFailureFact::Response(failure) => {
                ClientFailure::new(failure.clone(), 400, true)
            }
        }
    }

    pub(super) fn decision(
        observation: &AttemptObservation,
        classified: ClassifiedCyberPolicyFailure<'_>,
    ) -> AttemptDecision {
        let client_failure = Self::client_failure(&classified);
        if observation.routing.can_retry_next_candidate {
            AttemptDecision::RetryNextCandidate {
                exhaustion: None,
                on_exhaustion: Some(client_failure),
            }
        } else {
            AttemptDecision::Return(AttemptReturnKind::Failed(client_failure))
        }
    }

    pub(in crate::dispatch) async fn prepare(
        &self,
        request: &CodexResponsesRequest,
    ) -> CyberPolicyScope {
        let Some(session_key) = session_key(request) else {
            return CyberPolicyScope {
                session_key: None,
                state: SessionState::default(),
            };
        };
        let state = match timeout(STATE_IO_TIMEOUT, self.store.load(&session_key)).await {
            Ok(Ok(state)) => state.unwrap_or_default(),
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "Failed to read cyber policy session state");
                SessionState::default()
            }
            Err(_) => {
                tracing::warn!("Timed out reading cyber policy session state");
                SessionState::default()
            }
        };
        CyberPolicyScope {
            session_key: Some(session_key),
            state,
        }
    }

    pub(super) async fn exclude_account(&self, plan: &CyberPolicyScope, account_id: &str) {
        let Some(session_key) = plan.session_key() else {
            return;
        };
        match timeout(
            STATE_IO_TIMEOUT,
            self.store
                .record_failure(session_key, account_id, SESSION_STATE_TTL),
        )
        .await
        {
            Ok(Ok(state)) => tracing::warn!(
                account_id,
                excluded_account_count = state.failed_account_ids.len(),
                "Excluded cyber policy account from this session"
            ),
            Ok(Err(error)) => tracing::warn!(
                account_id,
                error = %error,
                "Failed to record cyber policy session state"
            ),
            Err(_) => tracing::warn!(account_id, "Timed out recording cyber policy session state"),
        }
    }

    pub(in crate::dispatch) async fn observe_success(&self, plan: &CyberPolicyScope) {
        if !plan.has_failures() {
            return;
        }
        let Some(session_key) = plan.session_key() else {
            return;
        };
        match timeout(
            STATE_IO_TIMEOUT,
            self.store.clear(session_key, &plan.state.revision),
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

fn is_cyber_policy_code(code: Option<&str>) -> bool {
    code.is_some_and(|code| code.trim().eq_ignore_ascii_case("cyber_policy"))
}

fn failure_from_upstream_facts(facts: &UpstreamFailureFacts) -> ResponsesSseFailure {
    ResponsesSseFailure {
        event: "error".to_string(),
        message: facts.message.clone(),
        upstream_code: facts.code.clone(),
        upstream_type: None,
        explicit_status_code: facts.status_code,
        retry_after_seconds: facts.retry_after_seconds,
    }
}

pub(super) fn is_cyber_policy_failure(failure: &ResponsesSseFailure) -> bool {
    is_cyber_policy_code(failure.upstream_code.as_deref())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
