use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use tokio::sync::Mutex;

use crate::{
    codex::gateway::transport::types::CodexResponsesRequest,
    codex::{
        accounts::pool::AccountPool,
        events::event::EventLevel,
        gateway::transport::{
            usage_events::extract_sse_usage,
            websocket::{
                append_rate_limit_updates, latest_turn_state, SharedRateLimitUpdates,
                SharedTurnState,
            },
        },
    },
};

use super::{
    evict_reasoning_replay_with_deps,
    limits::apply_rate_limit_headers_with_deps,
    log_codex_upstream_response_with_deps, record_response_affinity_with_deps,
    record_usage_with_deps,
    stream::{ensure_stream_metadata, responses_sse_failure},
    CodexRequestLogContext, CodexUpstreamDependencies,
};

pub(super) struct StreamAudit {
    deps: CodexUpstreamDependencies,
    context: CodexRequestLogContext,
    account_slot: AccountSlotGuard,
    account_plan_type: Option<String>,
    request: CodexResponsesRequest,
    rate_limit_headers: Vec<(String, String)>,
}

impl StreamAudit {
    pub(super) fn new(
        deps: CodexUpstreamDependencies,
        context: CodexRequestLogContext,
        account_id: String,
        account_plan_type: Option<String>,
        request: CodexResponsesRequest,
        rate_limit_headers: Vec<(String, String)>,
    ) -> Self {
        let account_slot = AccountSlotGuard::new(deps.account_pool.clone(), account_id);
        Self {
            deps,
            context,
            account_slot,
            account_plan_type,
            request,
            rate_limit_headers,
        }
    }

    pub(super) async fn complete(&mut self, body: &[u8]) {
        apply_rate_limit_headers_with_deps(
            &self.deps,
            &self.context.account_id,
            self.account_plan_type.as_deref(),
            &self.rate_limit_headers,
        )
        .await;
        let body = String::from_utf8_lossy(body);
        let mut status = StatusCode::OK;
        let mut level = EventLevel::Info;
        let mut message = "v1 responses stream 已完成";
        let mut metadata = match extract_sse_usage(&body) {
            Ok(usage) => {
                if let Some(usage) = usage {
                    if record_usage_with_deps(
                        &self.deps,
                        &self.context.account_id,
                        usage,
                        self.request.expects_image_generation(),
                    )
                    .await
                    .is_err()
                    {
                        level = EventLevel::Warn;
                        message = "v1 responses stream 已完成但 usage 存储失败";
                        json!({"stream": true, "usage": usage, "usageStoreError": true})
                    } else {
                        json!({"stream": true, "usage": usage})
                    }
                } else {
                    json!({"stream": true, "usage": null})
                }
            }
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses stream 已完成但 SSE usage 无效";
                json!({"stream": true, "sseParseError": error.to_string()})
            }
        };
        match responses_sse_failure(&body) {
            Ok(Some(failure)) => {
                // SSE 响应头已发出，HTTP 状态不能回滚；用终止事件透传给客户端，并在审计里标记上游失败。
                status = StatusCode::BAD_GATEWAY;
                level = EventLevel::Error;
                message = "v1 responses stream 上游 SSE 失败";
                if failure.invalid_reasoning_replay() {
                    evict_reasoning_replay_with_deps(
                        &self.deps,
                        &self.request,
                        &self.context.account_id,
                    )
                    .await;
                }
                failure.extend_metadata(&mut metadata);
            }
            Ok(None) => {}
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses stream 已完成但 SSE 失败 metadata 无效";
                metadata = json!({"stream": true, "sseParseError": error.to_string()});
            }
        }
        ensure_stream_metadata(&mut metadata, true);
        log_codex_upstream_response_with_deps(
            &self.deps,
            &self.context,
            status,
            level,
            message,
            metadata,
        )
        .await;
        self.account_slot.release().await;
    }
}

pub(super) struct WebSocketStreamAudit {
    deps: CodexUpstreamDependencies,
    context: CodexRequestLogContext,
    account_slot: AccountSlotGuard,
    account_plan_type: Option<String>,
    request: CodexResponsesRequest,
    turn_state: Option<String>,
    rate_limit_headers: Vec<(String, String)>,
    rate_limit_updates: SharedRateLimitUpdates,
    turn_state_updates: SharedTurnState,
}

impl WebSocketStreamAudit {
    #[expect(
        clippy::too_many_arguments,
        reason = "stream audit captures one immutable snapshot from the upstream response boundary"
    )]
    pub(super) fn new(
        deps: CodexUpstreamDependencies,
        context: CodexRequestLogContext,
        account_id: String,
        account_plan_type: Option<String>,
        request: CodexResponsesRequest,
        turn_state: Option<String>,
        rate_limit_headers: Vec<(String, String)>,
        rate_limit_updates: SharedRateLimitUpdates,
        turn_state_updates: SharedTurnState,
    ) -> Self {
        let account_slot = AccountSlotGuard::new(deps.account_pool.clone(), account_id);
        Self {
            deps,
            context,
            account_slot,
            account_plan_type,
            request,
            turn_state,
            rate_limit_headers,
            rate_limit_updates,
            turn_state_updates,
        }
    }

    pub(super) async fn complete(&mut self, body: &[u8]) {
        let mut rate_limit_headers = self.rate_limit_headers.clone();
        append_rate_limit_updates(&mut rate_limit_headers, &self.rate_limit_updates).await;
        apply_rate_limit_headers_with_deps(
            &self.deps,
            &self.context.account_id,
            self.account_plan_type.as_deref(),
            &rate_limit_headers,
        )
        .await;
        let body = String::from_utf8_lossy(body).into_owned();
        let usage_result = extract_sse_usage(&body);
        let response_usage = match &usage_result {
            Ok(usage) => *usage,
            Err(_) => None,
        };
        let turn_state = latest_turn_state(&self.turn_state_updates)
            .await
            .or_else(|| self.turn_state.clone());
        record_response_affinity_with_deps(
            &self.deps,
            &self.request,
            &self.context.account_id,
            &body,
            turn_state.as_deref(),
            response_usage,
        )
        .await;

        let mut status = StatusCode::OK;
        let mut level = EventLevel::Info;
        let mut message = "v1 responses WebSocket stream 已完成";
        let mut metadata = match usage_result {
            Ok(Some(usage)) => {
                if record_usage_with_deps(
                    &self.deps,
                    &self.context.account_id,
                    usage,
                    self.request.expects_image_generation(),
                )
                .await
                .is_err()
                {
                    level = EventLevel::Warn;
                    message = "v1 responses WebSocket stream 已完成但 usage 存储失败";
                    json!({
                        "stream": true,
                        "transport": "websocket",
                        "usage": usage,
                        "rateLimitHeaders": self.rate_limit_headers.clone(),
                        "usageStoreError": true,
                    })
                } else {
                    json!({
                        "stream": true,
                        "transport": "websocket",
                        "usage": usage,
                        "rateLimitHeaders": self.rate_limit_headers.clone(),
                    })
                }
            }
            Ok(None) => json!({
                "stream": true,
                "transport": "websocket",
                "usage": null,
                "rateLimitHeaders": self.rate_limit_headers.clone(),
            }),
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses WebSocket stream 已完成但 SSE usage 无效";
                json!({
                    "stream": true,
                    "transport": "websocket",
                    "rateLimitHeaders": self.rate_limit_headers.clone(),
                    "sseParseError": error.to_string(),
                })
            }
        };
        match responses_sse_failure(&body) {
            Ok(Some(failure)) => {
                // SSE 响应头已经发给客户端，HTTP 状态不能回滚，只能在审计中标记上游失败。
                status = StatusCode::BAD_GATEWAY;
                level = EventLevel::Error;
                message = "v1 responses WebSocket stream 上游 SSE 失败";
                if failure.invalid_reasoning_replay() {
                    evict_reasoning_replay_with_deps(
                        &self.deps,
                        &self.request,
                        &self.context.account_id,
                    )
                    .await;
                }
                failure.extend_metadata(&mut metadata);
            }
            Ok(None) => {}
            Err(error) => {
                level = EventLevel::Warn;
                message = "v1 responses WebSocket stream 已完成但 SSE 失败 metadata 无效";
                metadata = json!({
                    "stream": true,
                    "transport": "websocket",
                    "rateLimitHeaders": self.rate_limit_headers.clone(),
                    "sseParseError": error.to_string(),
                });
            }
        }
        ensure_stream_metadata(&mut metadata, true);
        log_codex_upstream_response_with_deps(
            &self.deps,
            &self.context,
            status,
            level,
            message,
            metadata,
        )
        .await;
        self.account_slot.release().await;
    }
}

struct AccountSlotGuard {
    pool: Arc<Mutex<AccountPool>>,
    account_id: String,
    released: bool,
}

impl AccountSlotGuard {
    fn new(pool: Arc<Mutex<AccountPool>>, account_id: String) -> Self {
        Self {
            pool,
            account_id,
            released: false,
        }
    }

    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.pool.lock().await.release(&self.account_id);
        self.released = true;
    }
}

impl Drop for AccountSlotGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let pool = self.pool.clone();
        let account_id = self.account_id.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            pool.lock().await.release(&account_id);
        });
    }
}
