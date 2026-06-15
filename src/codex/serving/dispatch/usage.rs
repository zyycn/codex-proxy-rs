use crate::codex::{
    accounts::{pool::AccountWindowUsageDelta, repository::UsageDelta},
    gateway::transport::usage_events::TokenUsage,
};

use super::CodexUpstreamDependencies;

pub(super) async fn record_usage_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    usage: TokenUsage,
    image_generation_requested: bool,
) -> Result<(), ()> {
    let image_request_succeeded = image_generation_requested && usage.image_output_tokens > 0;
    let image_request_failed = image_generation_requested && !image_request_succeeded;
    deps.account_pool.lock().await.record_window_token_usage(
        account_id,
        AccountWindowUsageDelta {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_request_succeeded,
            image_request_failed,
        },
    );
    let Some(repo) = deps.account_repository.as_ref() else {
        return Ok(());
    };
    repo.record_usage(
        account_id,
        UsageDelta {
            input_tokens: u64_to_i64_saturating(usage.input_tokens),
            output_tokens: u64_to_i64_saturating(usage.output_tokens),
            cached_tokens: u64_to_i64_saturating(usage.cached_tokens),
            image_input_tokens: u64_to_i64_saturating(usage.image_input_tokens),
            image_output_tokens: u64_to_i64_saturating(usage.image_output_tokens),
            image_request_count: bool_to_i64(image_request_succeeded),
            image_request_failed_count: bool_to_i64(image_request_failed),
            empty_response_count: 0,
        },
    )
    .await
    .map_err(|_| ())
}

pub(super) async fn record_request_attempt(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    image_generation_requested: bool,
) -> Result<(), ()> {
    let Some(repo) = deps.account_repository.as_ref() else {
        return Ok(());
    };
    repo.record_usage(
        account_id,
        UsageDelta {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            image_request_count: 0,
            image_request_failed_count: bool_to_i64(image_generation_requested),
            empty_response_count: 0,
        },
    )
    .await
    .map_err(|_| ())
}

pub(super) async fn record_empty_response_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    image_generation_requested: bool,
) -> Result<(), ()> {
    let Some(repo) = deps.account_repository.as_ref() else {
        return Ok(());
    };
    repo.record_usage(
        account_id,
        UsageDelta {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            image_input_tokens: 0,
            image_output_tokens: 0,
            image_request_count: 0,
            image_request_failed_count: bool_to_i64(image_generation_requested),
            empty_response_count: 1,
        },
    )
    .await
    .map_err(|_| ())
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}
