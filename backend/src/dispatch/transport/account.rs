//! 调度层共享的账号级 Codex 上游调用。

use std::time::Instant;

use crate::{
    dispatch::affinity::AccountScopedRequest,
    fleet::account::Account,
    upstream::openai::transport::{
        CodexBackendClient, CodexBackendResponse, CodexBackendStreamingResponse, CodexClientError,
        CodexRequestContext,
    },
};

#[derive(Clone, Copy)]
pub(in crate::dispatch) struct AccountUpstreamContext<'a> {
    pub codex: &'a CodexBackendClient,
    pub request_id: &'a str,
    pub account: &'a Account,
    pub cookie_header: Option<&'a str>,
}

pub(in crate::dispatch) async fn create_response_with_account(
    context: AccountUpstreamContext<'_>,
    request: &AccountScopedRequest,
    started_at: Instant,
) -> Result<CodexBackendResponse, CodexClientError> {
    context
        .codex
        .create_response_with_pool_account_started_at(
            request.request(),
            codex_request_context(context, request),
            Some(&context.account.id),
            started_at,
        )
        .await
}

pub(in crate::dispatch) async fn create_response_stream_with_account(
    context: AccountUpstreamContext<'_>,
    request: &AccountScopedRequest,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    context
        .codex
        .create_response_stream_with_pool_account(
            request.request(),
            codex_request_context(context, request),
            Some(&context.account.id),
        )
        .await
}

fn codex_request_context<'a>(
    context: AccountUpstreamContext<'a>,
    request: &'a AccountScopedRequest,
) -> CodexRequestContext<'a> {
    let request_body = request.request();
    let identity = request.identity();
    CodexRequestContext {
        access_token: &context.account.access_token,
        account_id: context.account.account_id.as_deref(),
        request_id: context.request_id,
        turn_state: request_body.turn_state.as_deref(),
        turn_metadata: request_body.turn_metadata.as_deref(),
        beta_features: request_body.beta_features.as_deref(),
        include_timing_metrics: request_body.include_timing_metrics.as_deref(),
        version: request_body.version.as_deref(),
        codex_window_id: request_body.codex_window_id.as_deref(),
        parent_thread_id: request_body.parent_thread_id.as_deref(),
        cookie_header: context.cookie_header,
        installation_id: Some(&identity.installation_id),
        session_id: request_body.client_session_id.as_deref(),
        thread_id: request_body.client_thread_id.as_deref(),
        client_request_id: request_body.client_request_id.as_deref(),
        turn_id: request_body.client_turn_id.as_deref(),
    }
}
