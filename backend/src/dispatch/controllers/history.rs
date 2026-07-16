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
        protocol::responses::{
            CodexResponsesRequest, LocalReplayItem, LocalReplayTranscript, PreviousResponseScope,
            StreamCommitPolicy,
        },
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
    local_replay_transcript: Option<Arc<LocalReplayTranscript>>,
    local_replay_account_id: Option<String>,
    external_attempted_account_id: Option<String>,
    replay_owner_account: bool,
}

#[derive(Default)]
pub(in crate::dispatch) struct ConnectionReplaySnapshot {
    last_response_id: Option<String>,
    account_id: Option<String>,
    transcript: Option<Arc<LocalReplayTranscript>>,
}

pub(in crate::dispatch) struct ConnectionReplayPlan {
    update: ConnectionReplayUpdate,
}

enum ConnectionReplayUpdate {
    Replace,
    Append { history: Arc<LocalReplayTranscript> },
    Unavailable,
}

/// 已确认来源和目标账号不同的完整 transcript 重放边界。
pub(in crate::dispatch) struct CrossAccountReplay {
    source_account_id: String,
    target_account_id: String,
}

impl CrossAccountReplay {
    fn between(source_account_id: &str, target_account_id: &str) -> Option<Self> {
        (source_account_id != target_account_id).then(|| Self {
            source_account_id: source_account_id.to_string(),
            target_account_id: target_account_id.to_string(),
        })
    }

    pub(in crate::dispatch) fn validate_target(&self, account_id: &str) {
        debug_assert_ne!(self.source_account_id, self.target_account_id);
        debug_assert_eq!(self.target_account_id, account_id);
    }
}

pub(super) struct PreparedHistoryRequest {
    pub(super) request: CodexResponsesRequest,
    pub(super) cross_account_replay: Option<CrossAccountReplay>,
}

pub(in crate::dispatch) struct HistoryController;

pub(in crate::dispatch) fn sanitize_cross_account_input(request: &mut CodexResponsesRequest) {
    if request.input().is_empty() {
        return;
    }
    let input = request
        .input()
        .iter()
        .cloned()
        .filter_map(sanitize_cross_account_item)
        .collect();
    request.set_input(input);
}

impl HistoryController {
    pub(in crate::dispatch) fn new_connection_replay_snapshot() -> ConnectionReplaySnapshot {
        ConnectionReplaySnapshot::default()
    }

    pub(in crate::dispatch) fn prepare_connection_replay(
        snapshot: &ConnectionReplaySnapshot,
        request: &mut CodexResponsesRequest,
    ) -> ConnectionReplayPlan {
        let update = match request.previous_response_id() {
            None => {
                clear_local_replay(request);
                ConnectionReplayUpdate::Replace
            }
            Some(previous_response_id)
                if snapshot.last_response_id.as_deref() == Some(previous_response_id) =>
            {
                if let (Some(account_id), Some(history)) =
                    (snapshot.account_id.as_ref(), snapshot.transcript.as_ref())
                {
                    request.local_replay_transcript = Some(Arc::clone(history));
                    request.local_replay_account_id = Some(account_id.clone());
                    ConnectionReplayUpdate::Append {
                        history: Arc::clone(history),
                    }
                } else {
                    clear_local_replay(request);
                    ConnectionReplayUpdate::Unavailable
                }
            }
            Some(_) => {
                clear_local_replay(request);
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
        account_id: String,
        request_input: Vec<Value>,
        continued_from_previous_response: bool,
    ) {
        snapshot.last_response_id = Some(response_id);
        snapshot.account_id = Some(account_id.clone());
        match plan.update {
            ConnectionReplayUpdate::Replace => {
                snapshot.transcript = Some(Arc::new(transcript_for_turn(
                    request_input,
                    output,
                    &account_id,
                )));
            }
            ConnectionReplayUpdate::Append { mut history } => {
                if continued_from_previous_response {
                    append_turn(
                        Arc::make_mut(&mut history),
                        request_input,
                        output,
                        &account_id,
                    );
                    snapshot.transcript = Some(history);
                } else {
                    snapshot.transcript = Some(Arc::new(transcript_for_turn(
                        request_input,
                        output,
                        &account_id,
                    )));
                }
            }
            ConnectionReplayUpdate::Unavailable => snapshot.transcript = None,
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
        let (local_replay_transcript, local_replay_account_id) = match (
            request.local_replay_transcript.clone(),
            request.local_replay_account_id.clone(),
        ) {
            (Some(transcript), Some(account_id)) => (Some(transcript), Some(account_id)),
            (None, None) => (None, None),
            _ => {
                tracing::warn!(
                    previous_response_id,
                    "Ignoring incomplete connection-local replay provenance"
                );
                (None, None)
            }
        };
        let has_complete_local_replay = local_replay_transcript.is_some();
        let source = match (&managed_entry, has_complete_local_replay) {
            (Some(_), _) => HistorySource::Managed,
            (None, true) => HistorySource::ConnectionLocal,
            (None, false) => HistorySource::ExternalUnknown,
        };
        HistoryState {
            source,
            managed_entry,
            local_replay_transcript,
            local_replay_account_id,
            external_attempted_account_id: None,
            replay_owner_account: false,
        }
    }

    pub(super) fn preferred_account_id(state: &HistoryState) -> Option<&str> {
        state.local_replay_account_id.as_deref().or_else(|| {
            state
                .managed_entry
                .as_ref()
                .map(|entry| entry.account_id.as_str())
        })
    }

    pub(super) fn prepare_attempt(
        state: &mut HistoryState,
        request: &CodexResponsesRequest,
        account_id: &str,
    ) -> Result<PreparedHistoryRequest, String> {
        let prepared = match state.source {
            HistorySource::None => Some(PreparedHistoryRequest::plain(request.clone())),
            HistorySource::ExternalUnknown => {
                match state.external_attempted_account_id.as_deref() {
                    None => {
                        state.external_attempted_account_id = Some(account_id.to_string());
                        Some(PreparedHistoryRequest::plain(external_history_request(
                            request,
                        )))
                    }
                    Some(first_account_id) if first_account_id == account_id => Some(
                        PreparedHistoryRequest::plain(external_history_request(request)),
                    ),
                    Some(_) => None,
                }
            }
            HistorySource::ConnectionLocal => full_replay_from_state(state, request, account_id),
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
                HistorySource::Managed => state.local_replay_transcript.is_some(),
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
            && state.local_replay_transcript.is_some()
            && !state.replay_owner_account
    }
}

fn prepare_managed_attempt(
    state: &HistoryState,
    request: &CodexResponsesRequest,
    account_id: &str,
) -> Option<PreparedHistoryRequest> {
    let entry = state.managed_entry.as_ref()?;
    if entry.account_id != account_id || state.replay_owner_account {
        return replay_for_conversation(state, request, account_id, &entry.conversation_id);
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
            Some(PreparedHistoryRequest::plain(prepared))
        }
        PreviousResponseScope::ReplayRequired => {
            replay_for_conversation(state, request, account_id, &entry.conversation_id)
        }
        PreviousResponseScope::Unavailable => None,
    }
}

fn replay_for_conversation(
    state: &HistoryState,
    request: &CodexResponsesRequest,
    account_id: &str,
    conversation_id: &str,
) -> Option<PreparedHistoryRequest> {
    full_replay_from_state(state, request, account_id).map(|mut prepared| {
        prepared.request.local_conversation_id = Some(conversation_id.to_string());
        prepared
    })
}

fn full_replay_from_state(
    state: &HistoryState,
    request: &CodexResponsesRequest,
    account_id: &str,
) -> Option<PreparedHistoryRequest> {
    let transcript = state.local_replay_transcript.as_deref()?;
    let source_account_id = state.local_replay_account_id.as_deref()?;
    Some(full_replay_request(
        request,
        transcript,
        source_account_id,
        account_id,
    ))
}

impl HistoryState {
    fn empty() -> Self {
        Self {
            source: HistorySource::None,
            managed_entry: None,
            local_replay_transcript: None,
            local_replay_account_id: None,
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
    transcript: &LocalReplayTranscript,
    source_account_id: &str,
    target_account_id: &str,
) -> PreparedHistoryRequest {
    let cross_account_replay = CrossAccountReplay::between(source_account_id, target_account_id);
    let mut request = original.clone();
    let mut input = replay_input_for_account(transcript, target_account_id);
    input.reserve(original.input().len());
    input.extend_from_slice(original.input());
    request.set_input(input);
    request.set_previous_response_id(None);
    request.turn_state = None;
    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
    PreparedHistoryRequest {
        request,
        cross_account_replay,
    }
}

impl PreparedHistoryRequest {
    fn plain(request: CodexResponsesRequest) -> Self {
        Self {
            request,
            cross_account_replay: None,
        }
    }
}

fn clear_local_replay(request: &mut CodexResponsesRequest) {
    request.local_replay_transcript = None;
    request.local_replay_account_id = None;
}

fn transcript_for_turn(
    input: Vec<Value>,
    output: Vec<Value>,
    account_id: &str,
) -> LocalReplayTranscript {
    let mut transcript = LocalReplayTranscript::default();
    append_client_input(&mut transcript, input);
    append_account_output(&mut transcript, output, account_id);
    transcript
}

fn append_turn(
    transcript: &mut LocalReplayTranscript,
    input: Vec<Value>,
    output: Vec<Value>,
    account_id: &str,
) {
    project_transcript_to_account(transcript, account_id);
    append_client_input(transcript, input);
    append_account_output(transcript, output, account_id);
}

fn append_client_input(transcript: &mut LocalReplayTranscript, input: Vec<Value>) {
    transcript
        .items
        .extend(input.into_iter().map(LocalReplayItem::ClientInput));
}

fn append_account_output(
    transcript: &mut LocalReplayTranscript,
    output: Vec<Value>,
    account_id: &str,
) {
    transcript.items.extend(
        output
            .into_iter()
            .map(|item| LocalReplayItem::AccountOutput {
                account_id: account_id.to_string(),
                item,
            }),
    );
}

fn project_transcript_to_account(transcript: &mut LocalReplayTranscript, account_id: &str) {
    transcript.items = transcript
        .items
        .drain(..)
        .filter_map(|item| match item {
            LocalReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner != account_id => {
                sanitize_cross_account_item(item).map(LocalReplayItem::SanitizedOutput)
            }
            item => Some(item),
        })
        .collect();
}

fn replay_input_for_account(transcript: &LocalReplayTranscript, account_id: &str) -> Vec<Value> {
    transcript
        .items
        .iter()
        .filter_map(|item| match item {
            LocalReplayItem::ClientInput(value) | LocalReplayItem::SanitizedOutput(value) => {
                Some(value.clone())
            }
            LocalReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner == account_id => Some(without_output_id(item.clone())),
            LocalReplayItem::AccountOutput { item, .. } => {
                sanitize_cross_account_item(item.clone())
            }
        })
        .collect()
}

fn without_output_id(mut item: Value) -> Value {
    if let Value::Object(object) = &mut item {
        object.remove("id");
    }
    item
}

fn sanitize_cross_account_item(mut item: Value) -> Option<Value> {
    if let Value::Object(object) = &mut item {
        if matches!(
            object.get("type").and_then(Value::as_str),
            Some("compaction" | "compaction_summary" | "context_compaction")
        ) {
            return None;
        }
        object.remove("id");
        object.remove("encrypted_content");
    }
    Some(item)
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
