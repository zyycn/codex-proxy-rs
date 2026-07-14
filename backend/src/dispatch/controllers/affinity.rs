//! 会话亲和性路由与写入的唯一 feature owner。

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::timeout;

use crate::dispatch::affinity::{
    SessionAffinityEntry, SessionAffinityService, compute_variant_hash, hash_instructions,
};
use crate::upstream::openai::protocol::{
    events::TokenUsage,
    responses::{CodexResponsesRequest, completed_response_metadata},
};

const REDIS_BEST_EFFORT_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) struct AffinityController;

pub(super) struct StreamExit<'a> {
    pub affinity: &'a SessionAffinityService,
    pub conversation_id: Option<&'a str>,
    pub request: &'a CodexResponsesRequest,
    pub account_id: &'a str,
    pub body: &'a str,
    pub turn_state: Option<String>,
    pub usage: Option<TokenUsage>,
    pub completed: bool,
}

impl AffinityController {
    pub(super) async fn preferred_account_id(
        affinity: &SessionAffinityService,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        if request.previous_response_id().is_some() {
            return None;
        }
        let conversation_id = request
            .local_conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let variant_hash = compute_variant_hash(request);
        match timeout(
            REDIS_BEST_EFFORT_TIMEOUT,
            affinity.lookup_latest_account_by_conversation(
                conversation_id,
                None,
                Some(&variant_hash),
                now,
            ),
        )
        .await
        {
            Ok(account_id) => account_id,
            Err(_) => {
                tracing::warn!(
                    conversation_id,
                    timeout_ms = REDIS_BEST_EFFORT_TIMEOUT.as_millis(),
                    "Timed out reading conversation affinity; continuing without preference"
                );
                None
            }
        }
    }

    pub(super) async fn leave_complete(
        affinity: &SessionAffinityService,
        conversation_id: Option<&str>,
        request: &CodexResponsesRequest,
        account_id: &str,
        body: &str,
        turn_state: Option<String>,
        usage: Option<TokenUsage>,
    ) {
        record_response_affinity(
            affinity,
            conversation_id,
            request,
            account_id,
            body,
            turn_state,
            usage,
        )
        .await;
    }

    pub(super) async fn leave_stream(exit: StreamExit<'_>) {
        if !exit.completed {
            return;
        }
        record_response_affinity(
            exit.affinity,
            exit.conversation_id,
            exit.request,
            exit.account_id,
            exit.body,
            exit.turn_state,
            exit.usage,
        )
        .await;
    }
}

async fn record_response_affinity(
    affinity: &SessionAffinityService,
    conversation_id: Option<&str>,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<String>,
    usage: Option<TokenUsage>,
) {
    let metadata = match completed_response_metadata(body) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(error = %error, "Failed to parse completed response metadata for session affinity");
            return;
        }
    };
    let entry = SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: conversation_id.unwrap_or(&metadata.response_id).to_string(),
        turn_state: turn_state
            .filter(|value| !value.trim().is_empty())
            .or_else(|| request.turn_state.clone()),
        instructions_hash: Some(hash_instructions(Some(request.instructions()))),
        input_tokens: usage.map(|usage| usage.input_tokens),
        function_call_ids: metadata.function_call_ids,
        variant_hash: Some(compute_variant_hash(request)),
        continuation_scope: if request.store() {
            crate::upstream::openai::protocol::responses::PreviousResponseScope::Persisted
        } else {
            crate::upstream::openai::protocol::responses::PreviousResponseScope::ConnectionLocal
        },
        created_at: Utc::now(),
    };
    match timeout(
        REDIS_BEST_EFFORT_TIMEOUT,
        affinity.record(metadata.response_id.clone(), entry),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %error, response_id = %metadata.response_id, account_id, "Failed to record session affinity");
        }
        Err(_) => {
            tracing::warn!(
                response_id = %metadata.response_id,
                account_id,
                timeout_ms = REDIS_BEST_EFFORT_TIMEOUT.as_millis(),
                "Timed out recording session affinity"
            );
        }
    }
}
