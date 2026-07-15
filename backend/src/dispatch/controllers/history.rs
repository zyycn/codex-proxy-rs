//! previous response 所有权、重放与恢复规则的唯一 owner。

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use serde_json::Value;
use tokio::time::timeout;

use crate::{
    dispatch::{
        affinity::{SessionAffinityEntry, SessionAffinityService},
        lifecycle::contract::{
            AttemptDecision, AttemptObservation, AttemptObservationKind, AttemptReturnKind,
            AttemptRoutingFacts, CompleteResponseFacts, PinnedCandidateAcquireFailureKind,
        },
    },
    upstream::openai::{
        failure::{UpstreamFailureFacts, UpstreamFailureKind},
        protocol::responses::{CodexResponsesRequest, PreviousResponseScope, StreamCommitPolicy},
        transport::CodexBackendTransport,
        transport::websocket::PreviousResponseUnavailableReason,
    },
};

const REDIS_BEST_EFFORT_TIMEOUT: Duration = Duration::from_millis(100);
const CROSS_ACCOUNT_HISTORY_UNAVAILABLE_MESSAGE: &str =
    "previous response history cannot be sent to another account";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistorySource {
    None,
    Managed,
    ConnectionLocal,
    ExternalUnknown,
}

/// 单个请求内的 previous response 状态；策略只由 `HistoryController` 解释。
pub(super) struct HistoryState {
    source: HistorySource,
    managed_entry: Option<SessionAffinityEntry>,
    local_replay_input: Option<Arc<Vec<Value>>>,
    external_attempted_account_id: Option<String>,
    replay_owner_account: bool,
}

#[derive(Default)]
pub(in crate::dispatch) struct ConnectionReplaySnapshot {
    last_response_id: Option<String>,
    input: Option<Arc<Vec<Value>>>,
}

pub(in crate::dispatch) struct ConnectionReplayPlan {
    update: ConnectionReplayUpdate,
}

enum ConnectionReplayUpdate {
    Replace(Vec<Value>),
    Append {
        history: Arc<Vec<Value>>,
        input: Vec<Value>,
    },
    Unavailable,
}

pub(in crate::dispatch) struct HistoryController;

impl HistoryController {
    pub(in crate::dispatch) fn new_connection_replay_snapshot() -> ConnectionReplaySnapshot {
        ConnectionReplaySnapshot::default()
    }

    pub(in crate::dispatch) fn prepare_connection_replay(
        snapshot: &ConnectionReplaySnapshot,
        request: &mut CodexResponsesRequest,
    ) -> ConnectionReplayPlan {
        let turn_input = request.input().to_vec();
        let update = match request.previous_response_id() {
            None => {
                request.local_replay_input = None;
                ConnectionReplayUpdate::Replace(turn_input)
            }
            Some(previous_response_id)
                if snapshot.last_response_id.as_deref() == Some(previous_response_id) =>
            {
                if let Some(history) = snapshot.input.as_ref() {
                    request.local_replay_input = Some(Arc::clone(history));
                    ConnectionReplayUpdate::Append {
                        history: Arc::clone(history),
                        input: turn_input,
                    }
                } else {
                    request.local_replay_input = None;
                    ConnectionReplayUpdate::Unavailable
                }
            }
            Some(_) => {
                request.local_replay_input = None;
                ConnectionReplayUpdate::Unavailable
            }
        };
        request.local_replay_available = !matches!(update, ConnectionReplayUpdate::Unavailable);
        ConnectionReplayPlan { update }
    }

    pub(in crate::dispatch) fn commit_connection_replay(
        snapshot: &mut ConnectionReplaySnapshot,
        plan: ConnectionReplayPlan,
        response_id: String,
        output: Vec<Value>,
    ) {
        snapshot.last_response_id = Some(response_id);
        match plan.update {
            ConnectionReplayUpdate::Replace(input) => {
                snapshot.input = Some(Arc::new(sanitized_transcript(input, output)));
            }
            ConnectionReplayUpdate::Append { mut history, input } => {
                let mut turn = sanitized_transcript(input, output);
                Arc::make_mut(&mut history).append(&mut turn);
                snapshot.input = Some(history);
            }
            ConnectionReplayUpdate::Unavailable => snapshot.input = None,
        }
    }

    pub(super) async fn enter(
        affinity: &SessionAffinityService,
        request: &CodexResponsesRequest,
    ) -> HistoryState {
        let Some(previous_response_id) = request.previous_response_id() else {
            return HistoryState::empty();
        };
        let managed_entry = match timeout(
            REDIS_BEST_EFFORT_TIMEOUT,
            affinity.lookup(previous_response_id, Utc::now()),
        )
        .await
        {
            Ok(entry) => entry,
            Err(_) => {
                tracing::warn!(
                    previous_response_id,
                    timeout_ms = REDIS_BEST_EFFORT_TIMEOUT.as_millis(),
                    "Timed out reading previous response affinity; continuing without managed history"
                );
                None
            }
        };
        let local_replay_input = request.local_replay_input.clone();
        let source = match (&managed_entry, &local_replay_input) {
            (Some(_), _) => HistorySource::Managed,
            (None, Some(_)) => HistorySource::ConnectionLocal,
            (None, None) => HistorySource::ExternalUnknown,
        };
        HistoryState {
            source,
            managed_entry,
            local_replay_input,
            external_attempted_account_id: None,
            replay_owner_account: false,
        }
    }

    pub(super) fn preferred_account_id(state: &HistoryState) -> Option<&str> {
        state
            .managed_entry
            .as_ref()
            .map(|entry| entry.account_id.as_str())
    }

    pub(super) fn prepare_attempt(
        state: &mut HistoryState,
        request: &CodexResponsesRequest,
        account_id: &str,
    ) -> Result<CodexResponsesRequest, String> {
        let prepared = match state.source {
            HistorySource::None => Some(request.clone()),
            HistorySource::ExternalUnknown => {
                match state.external_attempted_account_id.as_deref() {
                    None => {
                        state.external_attempted_account_id = Some(account_id.to_string());
                        Some(external_history_request(request))
                    }
                    Some(first_account_id) if first_account_id == account_id => {
                        Some(external_history_request(request))
                    }
                    Some(_) => None,
                }
            }
            HistorySource::ConnectionLocal => state
                .local_replay_input
                .as_deref()
                .map(|replay_input| full_replay_request(request, replay_input)),
            HistorySource::Managed => prepare_managed_attempt(state, request, account_id),
        };
        prepared.ok_or_else(|| CROSS_ACCOUNT_HISTORY_UNAVAILABLE_MESSAGE.to_string())
    }

    pub(super) fn prepare_same_account_retry(state: &mut HistoryState, account_id: &str) -> bool {
        if !Self::can_retry_same_account(state, account_id) {
            return false;
        }
        state.replay_owner_account = true;
        true
    }

    pub(super) fn routing_facts(
        state: &HistoryState,
        account_id: Option<&str>,
    ) -> AttemptRoutingFacts {
        AttemptRoutingFacts {
            external_origin: state.source == HistorySource::ExternalUnknown,
            can_retry_same_account: account_id
                .is_some_and(|account_id| Self::can_retry_same_account(state, account_id)),
            can_retry_next_candidate: match state.source {
                HistorySource::None | HistorySource::ConnectionLocal => true,
                HistorySource::Managed => state.local_replay_input.is_some(),
                HistorySource::ExternalUnknown => false,
            },
        }
    }

    pub(super) fn conversation_id<'a>(
        state: &'a HistoryState,
        request: &'a CodexResponsesRequest,
    ) -> Option<&'a str> {
        state
            .managed_entry
            .as_ref()
            .map(|entry| entry.conversation_id.as_str())
            .or_else(|| non_empty(request.local_conversation_id.as_deref()))
    }

    pub(super) fn decide(observation: &AttemptObservation) -> Option<AttemptDecision> {
        if let AttemptObservationKind::PinnedCandidateUnavailable { kind, .. } = observation.kind {
            return Some(match kind {
                PinnedCandidateAcquireFailureKind::Busy => {
                    AttemptDecision::Return(AttemptReturnKind::ContinuationBusy)
                }
                PinnedCandidateAcquireFailureKind::Unavailable => {
                    AttemptDecision::Return(AttemptReturnKind::RouteUnavailable {
                        message: "the account that owns the previous response is unavailable for same-account recovery".to_string(),
                    })
                }
            });
        }
        let detail = match &observation.kind {
            AttemptObservationKind::UpstreamFailure(facts) if is_continuation_busy(facts) => {
                if observation.routing.can_retry_same_account {
                    return Some(AttemptDecision::RetrySameAccount);
                }
                return Some(AttemptDecision::Return(AttemptReturnKind::ContinuationBusy));
            }
            AttemptObservationKind::UpstreamFailure(facts) if is_history_failure(facts) => {
                facts.body.clone()
            }
            AttemptObservationKind::CompleteResponse(CompleteResponseFacts::Failed(failure))
            | AttemptObservationKind::StreamFailure(failure)
                if is_history_code(failure.upstream_code.as_deref()) =>
            {
                crate::dispatch::failure::sse::sse_failure_error_body(failure)
            }
            _ => return None,
        };

        if observation.routing.can_retry_same_account {
            return Some(AttemptDecision::RetrySameAccount);
        }
        if observation.routing.external_origin {
            return Some(AttemptDecision::Return(AttemptReturnKind::Observed));
        }
        Some(AttemptDecision::Return(
            AttemptReturnKind::RouteUnavailable { message: detail },
        ))
    }

    pub(in crate::dispatch) fn continuation_scope(
        request: &CodexResponsesRequest,
        transport: CodexBackendTransport,
        connection_local_continuation: bool,
    ) -> PreviousResponseScope {
        if request.store() {
            PreviousResponseScope::Persisted
        } else if transport == CodexBackendTransport::WebSocket && connection_local_continuation {
            PreviousResponseScope::ConnectionLocal
        } else if request.local_replay_available {
            PreviousResponseScope::ReplayRequired
        } else {
            PreviousResponseScope::Unavailable
        }
    }

    fn can_retry_same_account(state: &HistoryState, account_id: &str) -> bool {
        let Some(entry) = state.managed_entry.as_ref() else {
            return false;
        };
        entry.account_id == account_id
            && state.local_replay_input.is_some()
            && !state.replay_owner_account
    }
}

fn prepare_managed_attempt(
    state: &HistoryState,
    request: &CodexResponsesRequest,
    account_id: &str,
) -> Option<CodexResponsesRequest> {
    let entry = state.managed_entry.as_ref()?;
    if entry.account_id != account_id || state.replay_owner_account {
        return replay_for_conversation(state, request, &entry.conversation_id);
    }

    match entry.continuation_scope {
        PreviousResponseScope::Persisted
        | PreviousResponseScope::ConnectionLocal
        | PreviousResponseScope::ExternalUnknown => {
            let mut prepared = request.clone();
            prepared.local_conversation_id = Some(entry.conversation_id.clone());
            prepared.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
            prepared.previous_response_scope = Some(entry.continuation_scope);
            if prepared.turn_state.as_deref().is_none_or(str::is_empty) {
                prepared.turn_state.clone_from(&entry.turn_state);
            }
            Some(prepared)
        }
        PreviousResponseScope::ReplayRequired => {
            replay_for_conversation(state, request, &entry.conversation_id)
        }
        PreviousResponseScope::Unavailable => None,
    }
}

fn replay_for_conversation(
    state: &HistoryState,
    request: &CodexResponsesRequest,
    conversation_id: &str,
) -> Option<CodexResponsesRequest> {
    state.local_replay_input.as_deref().map(|replay_input| {
        let mut prepared = full_replay_request(request, replay_input);
        prepared.local_conversation_id = Some(conversation_id.to_string());
        prepared
    })
}

impl HistoryState {
    fn empty() -> Self {
        Self {
            source: HistorySource::None,
            managed_entry: None,
            local_replay_input: None,
            external_attempted_account_id: None,
            replay_owner_account: false,
        }
    }
}

fn external_history_request(original: &CodexResponsesRequest) -> CodexResponsesRequest {
    let mut request = original.clone();
    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
    request.previous_response_scope =
        Some(crate::upstream::openai::protocol::responses::PreviousResponseScope::ExternalUnknown);
    request
}

fn full_replay_request(
    original: &CodexResponsesRequest,
    replay_input: &[Value],
) -> CodexResponsesRequest {
    let mut request = original.clone();
    let mut input = Vec::with_capacity(replay_input.len() + original.input().len());
    input.extend_from_slice(replay_input);
    input.extend_from_slice(original.input());
    sanitize_replay_items(&mut input);
    request.set_input(input);
    request.set_previous_response_id(None);
    request.turn_state = None;
    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
    request
}

fn sanitized_transcript(mut input: Vec<Value>, mut output: Vec<Value>) -> Vec<Value> {
    sanitize_replay_items(&mut input);
    sanitize_replay_items(&mut output);
    input.append(&mut output);
    input
}

fn sanitize_replay_items(values: &mut [Value]) {
    for value in values {
        match value {
            Value::Array(values) => sanitize_replay_items(values),
            Value::Object(object) => {
                object.remove("id");
                object.remove("encrypted_content");
                for value in object.values_mut() {
                    sanitize_replay_items(std::slice::from_mut(value));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

fn is_continuation_busy(facts: &UpstreamFailureFacts) -> bool {
    matches!(
        facts.kind,
        UpstreamFailureKind::ContinuationUnavailable(
            PreviousResponseUnavailableReason::ConnectionBusy
        )
    )
}

fn is_history_failure(facts: &UpstreamFailureFacts) -> bool {
    facts
        .code
        .as_deref()
        .is_some_and(|code| is_history_code(Some(code)))
        || matches!(
            facts.kind,
            UpstreamFailureKind::ContinuationUnavailable(reason)
                if reason != PreviousResponseUnavailableReason::ConnectionBusy
        )
}

fn is_history_code(code: Option<&str>) -> bool {
    matches!(
        code,
        Some(
            "previous_response_not_found"
                | "invalid_encrypted_content"
                | "missing_tool_output"
                | "no_tool_output"
        )
    )
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
