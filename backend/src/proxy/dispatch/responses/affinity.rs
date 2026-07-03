use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex;

use crate::{
    proxy::dispatch::{
        reasoning_replay::ReasoningReplayCache,
        session_affinity::{
            compute_variant_hash, hash_instructions, RuntimeSessionAffinityService,
        },
    },
    upstream::protocol::{
        events::TokenUsage,
        responses::{completed_response_metadata, CodexResponsesRequest},
    },
};

pub(super) async fn record_response_affinity(
    session_affinity: &Arc<RuntimeSessionAffinityService>,
    reasoning_replay: &Arc<Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<String>,
    usage: Option<TokenUsage>,
) {
    let Some(conversation_id) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let metadata = match completed_response_metadata(body) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to parse completed response metadata for session affinity"
            );
            return;
        }
    };

    let variant_hash = compute_variant_hash(request);
    let entry = crate::proxy::dispatch::session_affinity::SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: conversation_id.to_string(),
        turn_state: turn_state
            .filter(|value| !value.trim().is_empty())
            .or_else(|| request.turn_state.clone()),
        instructions_hash: Some(hash_instructions(Some(&request.instructions))),
        input_tokens: usage.map(|usage| usage.input_tokens),
        function_call_ids: metadata.function_call_ids,
        variant_hash: Some(variant_hash.clone()),
        created_at: Utc::now(),
    };
    if let Err(error) = session_affinity
        .record(metadata.response_id.clone(), entry)
        .await
    {
        tracing::warn!(
            error = %error,
            response_id = %metadata.response_id,
            account_id = %account_id,
            "failed to record session affinity"
        );
    }

    reasoning_replay.lock().await.record(
        metadata.response_id,
        account_id,
        conversation_id,
        &variant_hash,
        &metadata.replay_items,
        Utc::now(),
    );
}

pub(super) async fn evict_reasoning_replay(
    reasoning_replay: &Arc<Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
) {
    let variant_hash = compute_variant_hash(request);
    let conversation_id = request
        .prompt_cache_key
        .as_deref()
        .unwrap_or("")
        .to_string();
    reasoning_replay.lock().await.evict_by_identity(
        account_id,
        &conversation_id,
        &variant_hash,
        Utc::now(),
    );
}
