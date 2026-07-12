//! previous response 所有权与连接内上下文重放。

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use crate::{
    dispatch::affinity::{SessionAffinityEntry, SessionAffinityService},
    upstream::openai::protocol::responses::{CodexResponsesRequest, StreamCommitPolicy},
};

/// 当前请求的 previous response 来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dispatch) enum HistorySource {
    None,
    Managed,
    ConnectionLocal,
    ExternalUnknown,
}

/// 请求级 previous response 恢复计划。
pub(in crate::dispatch) struct HistoryRecoveryPlan {
    source: HistorySource,
    managed_entry: Option<SessionAffinityEntry>,
    local_replay_input: Option<Arc<Vec<Value>>>,
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
                local_replay_input: None,
                external_attempted_account_id: None,
                replay_owner_account: false,
            };
        };
        let managed_entry = session_affinity
            .lookup(previous_response_id, Utc::now())
            .await;
        let local_replay_input = request.local_replay_input.clone();
        let source = match (&managed_entry, &local_replay_input) {
            (Some(_), _) => HistorySource::Managed,
            (None, Some(_)) => HistorySource::ConnectionLocal,
            (None, None) => HistorySource::ExternalUnknown,
        };
        Self {
            source,
            managed_entry,
            local_replay_input,
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
                    Some(external_history_request(original))
                }
                Some(first_account_id) if first_account_id == account_id => {
                    Some(external_history_request(original))
                }
                Some(_) => None,
            },
            HistorySource::ConnectionLocal => self
                .local_replay_input
                .as_deref()
                .map(|replay_input| full_replay_request(original, replay_input)),
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
                    self.local_replay_input.as_deref().map(|replay_input| {
                        let mut request = full_replay_request(original, replay_input);
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
        if entry.account_id != account_id
            || self.local_replay_input.is_none()
            || self.replay_owner_account
        {
            return false;
        }
        self.replay_owner_account = true;
        true
    }

    pub(in crate::dispatch) fn can_failover(&self) -> bool {
        match self.source {
            HistorySource::None | HistorySource::ConnectionLocal => true,
            HistorySource::Managed => self.local_replay_input.is_some(),
            HistorySource::ExternalUnknown => false,
        }
    }

    pub(in crate::dispatch) fn is_external_unknown(&self) -> bool {
        self.source == HistorySource::ExternalUnknown
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

pub(crate) fn sanitize_replay_items(values: &mut [Value]) {
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

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
