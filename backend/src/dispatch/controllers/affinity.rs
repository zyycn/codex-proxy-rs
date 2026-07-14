//! 会话亲和性路由与写入的唯一 feature owner。

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::timeout;

use crate::dispatch::affinity::{
    SessionAffinityEntry, SessionAffinityService, compute_variant_hash, hash_instructions,
};
use crate::upstream::openai::protocol::{
    events::TokenUsage,
    responses::{CodexResponsesRequest, PreviousResponseScope, completed_response_metadata},
};

const REDIS_BEST_EFFORT_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) struct AffinityController;

pub(super) struct ResponseExit<'a> {
    pub affinity: &'a SessionAffinityService,
    pub conversation_id: Option<&'a str>,
    pub request: &'a CodexResponsesRequest,
    pub account_id: &'a str,
    pub body: &'a str,
    pub turn_state: Option<String>,
    pub usage: Option<TokenUsage>,
    pub continuation_scope: PreviousResponseScope,
}

pub(super) struct StreamExit<'a> {
    pub response: ResponseExit<'a>,
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

    pub(super) async fn leave_complete(exit: ResponseExit<'_>) {
        record_response_affinity(exit).await;
    }

    pub(super) async fn leave_stream(exit: StreamExit<'_>) {
        if !exit.completed {
            return;
        }
        record_response_affinity(exit.response).await;
    }
}

async fn record_response_affinity(exit: ResponseExit<'_>) {
    let ResponseExit {
        affinity,
        conversation_id,
        request,
        account_id,
        body,
        turn_state,
        usage,
        continuation_scope,
    } = exit;
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
        continuation_scope,
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
