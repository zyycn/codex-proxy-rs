//! 流式 Responses 在提交前共享的 request/attempt 生命周期。

use std::{sync::Arc, time::Instant};

use crate::{
    dispatch::{
        controllers::{
            ControllerSet, DispatchErrorObservation, PrefetchedStreamFailureObservation,
            StreamControllerContext,
        },
        errors::{ResponseDispatchError, backend_transport_name},
        lifecycle::{
            RequestContext, RequestEnterDependencies, RequestMode,
            attempt::{AttemptMode, AttemptRunnerDependencies},
            contract::{
                AttemptContractError, EstablishedAttemptContext, EstablishedResponse,
                EstablishedStream,
            },
            enter_request,
            finalizer::StreamFinalizer,
            pipeline::{AttemptPipelineOutcome, run_attempt_pipeline},
        },
        service::{ResponseDispatchService, ResponseDispatchStream},
        stream::live::spawn_live_response_stream,
    },
    upstream::openai::protocol::{responses::CodexResponsesRequest, sse::SseError},
};

impl ResponseDispatchService {
    /// 调度流式 Responses 请求到 Codex Responses 上游。
    pub async fn stream(
        &self,
        request_id: &str,
        route: &str,
        request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<ResponseDispatchStream, ResponseDispatchError> {
        let started_at = Instant::now();
        let controllers = ControllerSet::new(
            Arc::clone(&self.session_affinity),
            Arc::clone(&self.account_pool),
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
            RequestMode::Stream,
        )
        .await;
        let RequestContext {
            request,
            display_model,
            compact,
            tuple_schema,
            controllers,
            controller_scope,
            candidates,
            ..
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
            AttemptMode::Stream {
                tuple_schema: tuple_schema.clone(),
            },
            request,
            controller_scope,
            candidates,
        )
        .await
        .map_err(attempt_contract_error)?
        {
            AttemptPipelineOutcome::Established(established) => match established {
                EstablishedResponse::Stream(established) => *established,
                EstablishedResponse::Complete(_) => return Err(attempt_mode_mismatch()),
            },
            AttemptPipelineOutcome::Rejected(rejection) => {
                let rejection = *rejection;
                if let (Some(account), Some(attempt_request), Some(attempt), Some(stream_failure)) = (
                    rejection.account.as_ref(),
                    rejection.attempt_request.as_ref(),
                    rejection.attempt.as_ref(),
                    rejection.stream_failure.as_ref(),
                ) {
                    controllers
                        .observe_prefetched_stream_failure(PrefetchedStreamFailureObservation {
                            request_id,
                            account_id: &account.id,
                            route,
                            model: &display_model,
                            requested_model,
                            started_at,
                            transport: rejection.transport,
                            request: attempt_request,
                            failure: &stream_failure.failure,
                            error: &rejection.error,
                            diagnostics: &stream_failure.diagnostics,
                            rate_limit_headers: &stream_failure.rate_limit_headers,
                            prefetched: &stream_failure.prefetched,
                            trace: &rejection.trace,
                            attempt,
                        })
                        .await;
                }
                controllers
                    .observe_dispatch_error(
                        DispatchErrorObservation {
                            request_id,
                            client_api_key_id: rejection.request.client_api_key_id.as_deref(),
                            account_id: rejection.account_id.as_deref(),
                            route,
                            model: requested_model,
                            started_at,
                            stream: true,
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

        let EstablishedStream {
            context,
            lease,
            response,
            decoder,
            initial_batch,
        } = established;
        let EstablishedAttemptContext {
            request,
            controller_scope,
            trace,
            account,
            attempt,
            ..
        } = context;
        self.models
            .observe_models_etag(response.response_metadata.models_etag.as_deref());
        let attempts = trace.attempts().to_vec();
        let transport = response.transport;
        let finalizer = StreamFinalizer::new(
            controllers,
            controller_scope,
            StreamControllerContext {
                account_id: account.id,
                account_plan_type: account.plan_type,
                request_id: request_id.to_string(),
                route: route.to_string(),
                display_model,
                requested_model: requested_model.to_string(),
                request,
                transport,
                set_cookie_headers: response.set_cookie_headers,
                rate_limit_headers: response.rate_limit_headers,
                rate_limit_header_updates: response.rate_limit_header_updates,
                turn_state_update: response.turn_state_update,
                websocket_pool_decision: response.websocket_pool_decision,
                turn_state: response.turn_state,
                diagnostics: response.diagnostics,
                response_metadata: response.response_metadata,
                attempt,
                attempts,
                started_at,
            },
            lease,
        );
        Ok(spawn_live_response_stream(
            finalizer,
            decoder,
            initial_batch,
            response.body,
            self.shutdown.clone(),
        ))
    }
}

fn attempt_contract_error(error: AttemptContractError) -> ResponseDispatchError {
    ResponseDispatchError::InvalidSse(SseError::ParseError(format!(
        "attempt lifecycle contract violation: {error}"
    )))
}

fn attempt_mode_mismatch() -> ResponseDispatchError {
    ResponseDispatchError::InvalidSse(SseError::ParseError(
        "attempt lifecycle returned a non-stream response for stream mode".to_string(),
    ))
}
