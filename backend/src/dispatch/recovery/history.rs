//! previous response 所有权与完整上下文重放。

use chrono::Utc;
use serde_json::Value;

use crate::{
    dispatch::affinity::{
        types::ResponseReplaySnapshot, SessionAffinityEntry, SessionAffinityService,
    },
    upstream::openai::protocol::responses::{CodexResponsesRequest, StreamCommitPolicy},
};

/// 当前请求的 previous response 来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dispatch) enum HistorySource {
    None,
    Managed,
    ExternalUnknown,
}

/// 请求级 previous response 恢复计划。
pub(in crate::dispatch) struct HistoryRecoveryPlan {
    source: HistorySource,
    managed_entry: Option<SessionAffinityEntry>,
    external_attempted_account_id: Option<String>,
    replay_owner_account: bool,
}

impl HistoryRecoveryPlan {
    pub(in crate::dispatch) async fn load(
        session_affinity: &SessionAffinityService,
        request: &CodexResponsesRequest,
    ) -> Self {
        let Some(previous_response_id) = request.previous_response_id() else {
            return Self {
                source: HistorySource::None,
                managed_entry: None,
                external_attempted_account_id: None,
                replay_owner_account: false,
            };
        };
        let managed_entry = session_affinity
            .lookup(previous_response_id, Utc::now())
            .await;
        Self {
            source: if managed_entry.is_some() {
                HistorySource::Managed
            } else {
                HistorySource::ExternalUnknown
            },
            managed_entry,
            external_attempted_account_id: None,
            replay_owner_account: false,
        }
    }

    pub(in crate::dispatch) fn preferred_account_id(&self) -> Option<&str> {
        self.managed_entry
            .as_ref()
            .map(|entry| entry.account_id.as_str())
    }

    pub(in crate::dispatch) fn prepare_attempt(
        &mut self,
        original: &CodexResponsesRequest,
        account_id: &str,
    ) -> Option<CodexResponsesRequest> {
        match self.source {
            HistorySource::None => Some(original.clone()),
            HistorySource::ExternalUnknown => match self.external_attempted_account_id.as_deref() {
                None => {
                    self.external_attempted_account_id = Some(account_id.to_string());
                    let mut request = original.clone();
                    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
                    request.previous_response_scope = Some(
                            crate::upstream::openai::protocol::responses::PreviousResponseScope::ExternalUnknown,
                        );
                    Some(request)
                }
                Some(first_account_id) if first_account_id == account_id => {
                    let mut request = original.clone();
                    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
                    request.previous_response_scope = Some(
                            crate::upstream::openai::protocol::responses::PreviousResponseScope::ExternalUnknown,
                        );
                    Some(request)
                }
                Some(_) => None,
            },
            HistorySource::Managed => {
                let entry = self.managed_entry.as_ref()?;
                if entry.account_id == account_id && !self.replay_owner_account {
                    let mut request = original.clone();
                    request.local_conversation_id = Some(entry.conversation_id.clone());
                    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
                    request.previous_response_scope = Some(entry.continuation_scope);
                    if request.turn_state.as_deref().is_none_or(str::is_empty) {
                        request.turn_state.clone_from(&entry.turn_state);
                    }
                    Some(request)
                } else {
                    entry.replay.as_ref().map(|snapshot| {
                        let mut request = full_replay_request(original, snapshot);
                        request.local_conversation_id = Some(entry.conversation_id.clone());
                        request
                    })
                }
            }
        }
    }

    /// previous ID 在原连接不可用时，切换为同账号完整重放。
    pub(in crate::dispatch) fn recover_managed_history(&mut self, account_id: &str) -> bool {
        let Some(entry) = self.managed_entry.as_ref() else {
            return false;
        };
        if entry.account_id != account_id || entry.replay.is_none() || self.replay_owner_account {
            return false;
        }
        self.replay_owner_account = true;
        true
    }

    pub(in crate::dispatch) fn can_failover(&self) -> bool {
        !matches!(self.source, HistorySource::ExternalUnknown)
            && self
                .managed_entry
                .as_ref()
                .is_none_or(|entry| entry.replay.is_some())
    }

    pub(in crate::dispatch) fn is_external_unknown(&self) -> bool {
        self.source == HistorySource::ExternalUnknown
    }

    pub(in crate::dispatch) fn completed_replay(
        &self,
        original: &CodexResponsesRequest,
        output: &[Value],
    ) -> Option<ResponseReplaySnapshot> {
        let mut full_input = match self.source {
            HistorySource::None => original.input().to_vec(),
            HistorySource::Managed => self
                .managed_entry
                .as_ref()?
                .replay
                .as_ref()?
                .full_input
                .clone(),
            HistorySource::ExternalUnknown => return None,
        };
        if matches!(self.source, HistorySource::Managed) {
            full_input.extend_from_slice(original.input());
        }
        full_input.extend_from_slice(output);
        Some(ResponseReplaySnapshot { full_input })
    }

    pub(in crate::dispatch) fn conversation_id<'a>(
        &'a self,
        original: &'a CodexResponsesRequest,
    ) -> Option<&'a str> {
        self.managed_entry
            .as_ref()
            .map(|entry| entry.conversation_id.as_str())
            .or_else(|| non_empty(original.local_conversation_id.as_deref()))
    }
}

fn full_replay_request(
    original: &CodexResponsesRequest,
    snapshot: &ResponseReplaySnapshot,
) -> CodexResponsesRequest {
    let mut request = original.clone();
    let mut input = snapshot.full_input.clone();
    input.extend_from_slice(original.input());
    strip_account_bound_encrypted_content(&mut input);
    request.set_input(input);
    request.set_previous_response_id(None);
    request.turn_state = None;
    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
    request
}

fn strip_account_bound_encrypted_content(values: &mut [Value]) {
    for value in values {
        match value {
            Value::Array(values) => strip_account_bound_encrypted_content(values),
            Value::Object(object) => {
                object.remove("encrypted_content");
                for value in object.values_mut() {
                    strip_account_bound_encrypted_content(std::slice::from_mut(value));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
