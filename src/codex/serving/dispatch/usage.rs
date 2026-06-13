use crate::{
    codex::accounts::repository::UsageDelta, codex::gateway::transport::usage::TokenUsage,
};

use super::CodexUpstreamDependencies;

pub(super) async fn record_usage_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
    usage: TokenUsage,
) -> Result<(), ()> {
    let Some(repo) = deps.account_repository.as_ref() else {
        return Ok(());
    };
    repo.record_usage(
        account_id,
        UsageDelta {
            input_tokens: u64_to_i64_saturating(usage.input_tokens),
            output_tokens: u64_to_i64_saturating(usage.output_tokens),
            cached_tokens: u64_to_i64_saturating(usage.cached_tokens),
            empty_response_count: 0,
        },
    )
    .await
    .map_err(|_| ())
}

pub(super) async fn record_request_attempt(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
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
            empty_response_count: 0,
        },
    )
    .await
    .map_err(|_| ())
}

pub(super) async fn record_empty_response_with_deps(
    deps: &CodexUpstreamDependencies,
    account_id: &str,
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
            empty_response_count: 1,
        },
    )
    .await
    .map_err(|_| ())
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}
