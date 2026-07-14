//! 调度层共享的账号级 Codex 上游调用。

use std::time::Instant;

use crate::{
    dispatch::affinity::AccountIdentityService,
    fleet::account::Account,
    upstream::openai::{
        protocol::responses::CodexResponsesRequest,
        transport::{
            CodexBackendClient, CodexBackendResponse, CodexBackendStreamingResponse,
            CodexClientError, CodexRequestContext,
        },
    },
};

#[derive(Clone, Copy)]
pub(in crate::dispatch) struct AccountUpstreamContext<'a> {
    pub codex: &'a CodexBackendClient,
    pub account_identity: &'a AccountIdentityService,
    pub request_id: &'a str,
    pub account: &'a Account,
    pub cookie_header: Option<&'a str>,
}

pub(in crate::dispatch) async fn create_response_with_account(
    context: AccountUpstreamContext<'_>,
    request: &CodexResponsesRequest,
    started_at: Instant,
) -> Result<CodexBackendResponse, CodexClientError> {
    let identity = context
        .account_identity
        .scope(request, &context.account.id, context.request_id);
    context
        .codex
        .create_response_with_pool_account_started_at(
            request,
            CodexRequestContext {
                access_token: &context.account.access_token,
                account_id: context.account.account_id.as_deref(),
                request_id: context.request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: identity.parent_thread_id.as_deref(),
                cookie_header: context.cookie_header,
                installation_id: Some(&identity.installation_id),
                session_id: identity.session_id.as_deref(),
                thread_id: identity.thread_id.as_deref(),
                prompt_cache_key: identity.prompt_cache_key.as_deref(),
                client_request_id: Some(&identity.client_request_id),
                turn_id: identity.turn_id.as_deref(),
            },
            Some(&context.account.id),
            started_at,
        )
        .await
}

pub(in crate::dispatch) async fn create_response_stream_with_account(
    context: AccountUpstreamContext<'_>,
    request: &CodexResponsesRequest,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    let identity = context
        .account_identity
        .scope(request, &context.account.id, context.request_id);
    context
        .codex
        .create_response_stream_with_pool_account(
            request,
            CodexRequestContext {
                access_token: &context.account.access_token,
                account_id: context.account.account_id.as_deref(),
                request_id: context.request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: identity.parent_thread_id.as_deref(),
                cookie_header: context.cookie_header,
                installation_id: Some(&identity.installation_id),
                session_id: identity.session_id.as_deref(),
                thread_id: identity.thread_id.as_deref(),
                prompt_cache_key: identity.prompt_cache_key.as_deref(),
                client_request_id: Some(&identity.client_request_id),
                turn_id: identity.turn_id.as_deref(),
            },
            Some(&context.account.id),
        )
        .await
}
