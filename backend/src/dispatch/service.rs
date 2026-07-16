//! Responses 创建编排与调度服务。
//!
//! 包含了将 OpenAI 请求调度到 Codex 上游账号的完整逻辑，包括：
//! - 响应创建（非流式 / 流式）
//! - 会话亲和性与隐式续接
//! - reasoning replay
//! - 账号回退与错误恢复
//! - 配额验证

use std::{pin::Pin, sync::Arc, time::Instant};

use bytes::Bytes;
use futures::stream::Stream;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    dispatch::{
        affinity::{AccountIdentityService, SessionAffinityService},
        controllers::{
            CompleteExit, ControllerSet, DispatchErrorObservation,
            cloudflare::CloudflareRecovery,
            history::{
                ConnectionReplayPlan as HistoryConnectionReplayPlan,
                ConnectionReplaySnapshot as HistoryConnectionReplaySnapshot, HistoryController,
            },
        },
        errors::backend_transport_name,
        lifecycle::{
            RequestContext, RequestEnterDependencies, RequestMode,
            attempt::{AttemptMode, AttemptRunnerDependencies},
            contract::{
                AttemptContractError, EstablishedAttemptContext, EstablishedComplete,
                EstablishedCompleteBody, EstablishedResponse, FinalOutcome,
            },
            enter_request,
            pipeline::{AttemptPipelineOutcome, run_attempt_pipeline},
        },
        transport::canonical::CanonicalResponseEvent,
    },
    fleet::pool::AccountPoolService,
    models::service::ModelService,
    telemetry::recorder::Recorder,
    upstream::openai::{
        protocol::{responses::CodexResponsesRequest, sse::SseError},
        transport::CodexBackendClient,
    },
};

use super::errors::{ResponseDispatchError, ResponseDispatchStreamError};

/// OpenAI Responses 调度服务。
#[derive(Clone)]
pub struct ResponseDispatchService {
    pub(in crate::dispatch) account_pool: Arc<AccountPoolService>,
    pub(in crate::dispatch) models: Arc<ModelService>,
    pub(in crate::dispatch) codex: Arc<CodexBackendClient>,
    pub(in crate::dispatch) session_affinity: Arc<SessionAffinityService>,
    pub(in crate::dispatch) account_identity: Arc<AccountIdentityService>,
    pub(in crate::dispatch) recorder: Arc<Recorder>,
    pub(in crate::dispatch) cloudflare: CloudflareRecovery,
    pub(in crate::dispatch) shutdown: CancellationToken,
}

pub(crate) struct ResponseDispatchServiceParts {
    pub account_pool: Arc<AccountPoolService>,
    pub models: Arc<ModelService>,
    pub codex: Arc<CodexBackendClient>,
    pub session_affinity: Arc<SessionAffinityService>,
    pub account_identity: Arc<AccountIdentityService>,
    pub recorder: Arc<Recorder>,
    pub cloudflare: CloudflareRecovery,
    pub shutdown: CancellationToken,
}

/// Responses live SSE 响应体流。
pub type ResponseBodyStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, ResponseDispatchStreamError>> + Send + 'static>>;

/// Responses live SSE 调度结果。
pub struct ResponseDispatchStream {
    pub body: ResponseBodyStream,
    pub(crate) canonical_events: tokio::sync::mpsc::UnboundedReceiver<Vec<CanonicalResponseEvent>>,
    pub response_headers: Vec<(String, String)>,
}

/// Responses 非流式调度结果。
pub struct ResponseDispatchResponse {
    pub body: Value,
    pub response_headers: Vec<(String, String)>,
}

/// 入站 Responses WebSocket 连接持有的窄重放快照。
pub(crate) struct ConnectionReplaySnapshot {
    inner: HistoryConnectionReplaySnapshot,
}

/// 单次 `response.create` 对快照的待提交重放计划。
pub(crate) struct ConnectionReplayPlan {
    inner: HistoryConnectionReplayPlan,
}

/// API 从 canonical 响应事件采集的原始 transcript 事实。
pub(crate) struct ConnectionTranscriptFacts {
    response_id: String,
    output: Vec<Value>,
}

impl ConnectionTranscriptFacts {
    pub(crate) fn new(response_id: String, output: Vec<Value>) -> Self {
        Self {
            response_id,
            output,
        }
    }
}

fn attempt_contract_error(error: AttemptContractError) -> ResponseDispatchError {
    ResponseDispatchError::InvalidSse(SseError::ParseError(format!(
        "attempt lifecycle contract violation: {error}"
    )))
}

fn attempt_mode_mismatch(expected: &str) -> ResponseDispatchError {
    ResponseDispatchError::InvalidSse(SseError::ParseError(format!(
        "attempt lifecycle returned a response for the wrong mode; expected {expected}"
    )))
}

impl ResponseDispatchService {
    pub(crate) fn new(parts: ResponseDispatchServiceParts) -> Self {
        Self {
            account_pool: parts.account_pool,
            models: parts.models,
            codex: parts.codex,
            session_affinity: parts.session_affinity,
            account_identity: parts.account_identity,
            recorder: parts.recorder,
            cloudflare: parts.cloudflare,
            shutdown: parts.shutdown,
        }
    }

    pub(crate) fn connection_replay_snapshot(&self) -> ConnectionReplaySnapshot {
        ConnectionReplaySnapshot {
            inner: HistoryController::new_connection_replay_snapshot(),
        }
    }

    pub(crate) fn prepare_connection_replay(
        &self,
        snapshot: &ConnectionReplaySnapshot,
        request: &mut CodexResponsesRequest,
    ) -> ConnectionReplayPlan {
        ConnectionReplayPlan {
            inner: HistoryController::prepare_connection_replay(&snapshot.inner, request),
        }
    }

    pub(crate) fn commit_connection_replay(
        &self,
        snapshot: &mut ConnectionReplaySnapshot,
        plan: ConnectionReplayPlan,
        transcript: ConnectionTranscriptFacts,
    ) {
        HistoryController::commit_connection_replay(
            &mut snapshot.inner,
            plan.inner,
            transcript.response_id,
            transcript.output,
        );
    }

    /// 调度非流式 Responses 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        route: &str,
        request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<ResponseDispatchResponse, ResponseDispatchError> {
        let started_at = Instant::now();
        let controllers = ControllerSet::new(
            Arc::clone(&self.session_affinity),
            Arc::clone(&self.account_pool),
            Arc::clone(&self.codex),
            self.cloudflare.clone(),
            Arc::clone(&self.recorder),
        );
        let context = enter_request(
            RequestEnterDependencies {
                account_pool: &self.account_pool,
                models: &self.models,
                account_identity: &self.account_identity,
                controllers,
            },
            request,
            requested_model,
            RequestMode::Complete,
        )
        .await;
        let RequestContext {
            request,
            display_model,
            compact,
            tuple_schema,
            image_generation_requested,
            controllers,
            controller_scope,
            candidates,
        } = context;
        let established = match run_attempt_pipeline(
            AttemptRunnerDependencies {
                account_pool: self.account_pool.as_ref(),
                codex: self.codex.as_ref(),
                controllers: &controllers,
                account_identity: self.account_identity.as_ref(),
                request_id,
                route,
                requested_model,
                started_at,
            },
            AttemptMode::Complete { tuple_schema },
            request,
            controller_scope,
            candidates,
        )
        .await
        .map_err(attempt_contract_error)?
        {
            AttemptPipelineOutcome::Established(established) => match established {
                EstablishedResponse::Complete(established) => *established,
                EstablishedResponse::Stream(_) => {
                    return Err(attempt_mode_mismatch("complete"));
                }
            },
            AttemptPipelineOutcome::Rejected(rejection) => {
                let rejection = *rejection;
                controllers
                    .observe_dispatch_error(
                        DispatchErrorObservation {
                            request_id,
                            client_api_key_id: rejection.request.client_api_key_id.as_deref(),
                            account_id: rejection.account_id.as_deref(),
                            route,
                            model: requested_model,
                            started_at,
                            stream: false,
                            compact,
                            transport: Some(backend_transport_name(rejection.transport)),
                        },
                        &rejection.error,
                    )
                    .await;
                let error = rejection.error;
                return Err(error);
            }
        };
        let EstablishedComplete {
            context,
            response,
            body,
        } = established;
        let EstablishedAttemptContext {
            request,
            controller_scope,
            trace,
            account,
            attempt,
            ..
        } = context;
        let (outcome, body) = match body {
            EstablishedCompleteBody::Completed(body) => (FinalOutcome::Completed, body),
            EstablishedCompleteBody::Incomplete(body) => (FinalOutcome::Incomplete, body),
        };
        let response_id = body.get("id").and_then(Value::as_str);
        let completed = outcome == FinalOutcome::Completed;
        controllers
            .finalize_complete(
                controller_scope,
                CompleteExit {
                    request: &request,
                    account_id: &account.id,
                    body: &response.body,
                    turn_state: response.turn_state.clone(),
                    usage: response.usage,
                    image_generation_requested,
                    completed,
                    recorder: &self.recorder,
                    models: &self.models,
                    request_id,
                    route,
                    requested_model,
                    display_model: &display_model,
                    started_at,
                    response_id,
                    response: &response,
                    trace: &trace,
                    attempt: &attempt,
                },
                outcome,
            )
            .await;
        Ok(ResponseDispatchResponse {
            body,
            response_headers: response.response_metadata.client_headers,
        })
    }
}
