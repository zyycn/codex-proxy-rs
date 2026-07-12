//! previous response 所有权与完整上下文重放。

use chrono::Utc;
use serde_json::Value;

use crate::{
    dispatch::affinity::{
        service::{MAX_REPLAY_DEPTH, MAX_REPLAY_SESSION_BYTES, MAX_REPLAY_SNAPSHOT_BYTES},
        types::ResponseReplaySnapshot,
        SessionAffinityEntry, SessionAffinityService,
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
    replay_input: Option<Vec<Value>>,
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
                replay_input: None,
                external_attempted_account_id: None,
                replay_owner_account: false,
            };
        };
        let managed_entry = session_affinity
            .lookup(previous_response_id, Utc::now())
            .await;
        let replay_input = match managed_entry.as_ref() {
            Some(entry) => {
                session_affinity
                    .replay_input(previous_response_id, entry, Utc::now())
                    .await
            }
            None => None,
        };
        Self {
            source: if managed_entry.is_some() {
                HistorySource::Managed
            } else {
                HistorySource::ExternalUnknown
            },
            managed_entry,
            replay_input,
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
                    self.replay_input.as_ref().map(|replay_input| {
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
            || self.replay_input.is_none()
            || self.replay_owner_account
        {
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
                .is_none_or(|_| self.replay_input.is_some())
    }

    pub(in crate::dispatch) fn is_external_unknown(&self) -> bool {
        self.source == HistorySource::ExternalUnknown
    }

    pub(in crate::dispatch) fn completed_replay(
        &self,
        original: &CodexResponsesRequest,
        output: &[Value],
    ) -> Option<ResponseReplaySnapshot> {
        let (parent_response_id, parent_depth, parent_bytes) = match self.source {
            HistorySource::None => (None, 0, 0),
            HistorySource::Managed => {
                self.replay_input.as_ref()?;
                let replay = self.managed_entry.as_ref()?.replay.as_ref()?;
                (
                    original.previous_response_id().map(ToString::to_string),
                    replay.depth,
                    replay.total_bytes,
                )
            }
            HistorySource::ExternalUnknown => return None,
        };
        let depth = parent_depth.checked_add(1)?;
        if depth > MAX_REPLAY_DEPTH {
            tracing::warn!(depth, "response replay depth limit reached");
            return None;
        }

        let mut turn_input = original.input().to_vec();
        let mut turn_output = output.to_vec();
        sanitize_replay_items(&mut turn_input);
        sanitize_replay_items(&mut turn_output);
        let Some(node_bytes) = limited_replay_json_bytes(&turn_input, &turn_output) else {
            tracing::warn!("response replay snapshot byte limit reached");
            return None;
        };
        let total_bytes = parent_bytes.checked_add(node_bytes)?;
        if total_bytes > MAX_REPLAY_SESSION_BYTES {
            tracing::warn!(
                node_bytes,
                total_bytes,
                "response replay session byte limit reached"
            );
            return None;
        }
        Some(ResponseReplaySnapshot {
            parent_response_id,
            turn_input,
            turn_output,
            depth,
            total_bytes,
        })
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
    replay_input: &[Value],
) -> CodexResponsesRequest {
    let mut request = original.clone();
    let mut input = replay_input.to_vec();
    input.extend_from_slice(original.input());
    sanitize_replay_items(&mut input);
    request.set_input(input);
    request.set_previous_response_id(None);
    request.turn_state = None;
    request.stream_commit_policy = StreamCommitPolicy::UntilOutputOrTerminal;
    request
}

fn sanitize_replay_items(values: &mut [Value]) {
    for value in values {
        match value {
            Value::Array(values) => sanitize_replay_items(values),
            Value::Object(object) => {
                object.remove("id");
                object.remove("encrypted_content");
                for value in object.values_mut() {
                    strip_encrypted_content(std::slice::from_mut(value));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

fn strip_encrypted_content(values: &mut [Value]) {
    for value in values {
        match value {
            Value::Array(values) => strip_encrypted_content(values),
            Value::Object(object) => {
                object.remove("encrypted_content");
                for value in object.values_mut() {
                    strip_encrypted_content(std::slice::from_mut(value));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

fn limited_replay_json_bytes(input: &[Value], output: &[Value]) -> Option<u64> {
    let mut writer = ReplaySizeWriter::default();
    serde_json::to_writer(&mut writer, &(input, output)).ok()?;
    Some(writer.written)
}

#[derive(Default)]
struct ReplaySizeWriter {
    written: u64,
}

impl std::io::Write for ReplaySizeWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next = self
            .written
            .checked_add(buffer.len() as u64)
            .filter(|next| *next <= MAX_REPLAY_SNAPSHOT_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "response replay snapshot byte limit reached",
                )
            })?;
        self.written = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
