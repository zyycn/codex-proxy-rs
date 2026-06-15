use axum::http::StatusCode;
use chrono::Utc;

use crate::codex::{
    accounts::{
        model::Account,
        pool::{AccountAcquireRequest, AcquiredAccount},
    },
    events::event::EventLevel,
    gateway::transport::types::CodexResponsesRequest,
};

use super::{
    fallback::{
        apply_upstream_account_retry_with_deps, build_account_exhaustion_detail,
        UpstreamAccountRetry, UpstreamRequestRecovery,
    },
    log_codex_upstream_response_with_deps, CodexRequestLogContext, CodexUpstreamDependencies,
    ImplicitResumeSnapshot,
};

pub(crate) enum UpstreamAccountRecoveryTransition {
    Retry(Box<AcquiredAccount>),
    Respond { status: StatusCode, message: String },
}

pub(super) async fn execute_upstream_account_recovery_transition_with_deps(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    retry: UpstreamAccountRetry,
    model: &str,
    excluded_account_ids: &mut Vec<String>,
    image_generation_requested: bool,
    upstream_message: String,
) -> UpstreamAccountRecoveryTransition {
    apply_upstream_account_retry_with_deps(deps, account, retry, image_generation_requested).await;
    excluded_account_ids.push(account.id.clone());
    let fallback = deps.account_pool.lock().await.acquire_with(
        AccountAcquireRequest::new(model, Utc::now())
            .with_exclude_account_ids(excluded_account_ids.iter().cloned()),
    );

    match fallback {
        Some(account) => UpstreamAccountRecoveryTransition::Retry(Box::new(account)),
        None => {
            let retry_message = retry.fallback_response_message(upstream_message);
            let message = fallback_exhausted_message_with_deps(deps, &retry_message).await;
            UpstreamAccountRecoveryTransition::Respond {
                status: retry.status(),
                message,
            }
        }
    }
}

pub(super) async fn execute_upstream_request_recovery_transition_with_deps(
    deps: &CodexUpstreamDependencies,
    request: &mut CodexResponsesRequest,
    recovery: UpstreamRequestRecovery,
    stream: bool,
    log_context: &CodexRequestLogContext,
    history_recovery_used: &mut bool,
    implicit_resume: &mut Option<ImplicitResumeSnapshot>,
) {
    *history_recovery_used = true;
    let stale_response_id = request.previous_response_id.clone();
    if let Some(response_id) = stale_response_id.as_deref() {
        deps.session_affinity.forget(response_id).await;
    }
    if let Some(snapshot) = implicit_resume.take() {
        snapshot.restore(request);
    }
    request.previous_response_id = None;
    request.turn_state = None;
    log_codex_upstream_response_with_deps(
        deps,
        log_context,
        StatusCode::BAD_REQUEST,
        EventLevel::Warn,
        "v1 responses 上游历史失效，去除 previous_response_id 后重试",
        recovery.metadata(stream, stale_response_id.as_deref()),
    )
    .await;
}

pub(super) async fn fallback_exhausted_message_with_deps(
    deps: &CodexUpstreamDependencies,
    message: &str,
) -> String {
    let summary = deps.account_pool.lock().await.status_summary(Utc::now());
    build_account_exhaustion_detail(summary, message)
}
