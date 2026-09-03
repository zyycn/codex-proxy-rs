//! OpenAI attempt 的选择、发送与响应流执行。

use super::*;

impl CodexProvider {
    pub(super) async fn execute_image(
        &self,
        image: &ImageRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if image.payload().protocol() != PROVIDER_NAME || candidate.upstream_model().is_some() {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let (endpoint_path, response_origin) = match image.kind() {
            ImageRequestKind::Generation => (
                CODEX_IMAGE_GENERATIONS_PATH,
                self.image_generations_url.clone(),
            ),
            ImageRequestKind::Edit => (CODEX_IMAGE_EDITS_PATH, self.image_edits_url.clone()),
        };
        let image_turn_id = image
            .payload()
            .context()
            .get("image_turn_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.execute_raw_json_endpoint(
            context,
            RawJsonEndpointRequest {
                response_origin,
                endpoint_path,
                body: image.payload().body().clone(),
                image_turn_id,
                turn_metadata: None,
            },
        )
        .await
    }

    pub(super) async fn execute_search(
        &self,
        search: &StandaloneSearchRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if search.payload().protocol() != PROVIDER_NAME || candidate.upstream_model().is_some() {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let turn_metadata = search
            .payload()
            .context()
            .get("turn_metadata")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.execute_raw_json_endpoint(
            context,
            RawJsonEndpointRequest {
                response_origin: self.search_url.clone(),
                endpoint_path: CODEX_ALPHA_SEARCH_PATH,
                body: search.payload().body().clone(),
                image_turn_id: None,
                turn_metadata,
            },
        )
        .await
    }

    async fn execute_raw_json_endpoint(
        &self,
        context: AttemptContext,
        request: RawJsonEndpointRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let selection_started_at = Instant::now();
        let lease = self
            .selector
            .select_for_provider_endpoint(&SelectCodexProviderEndpointCredential {
                request_url: &request.response_origin,
                attempt: &context,
            })
            .await
            .map_err(map_selection_error)?;
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let lease = Arc::new(lease);
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let provider_kind = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        let metadata = ProviderCallMetadata::for_provider_endpoint(
            provider_kind,
            lease.account_id().clone(),
            UpstreamTransport::new(HTTP_JSON_TRANSPORT).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        )
        .with_selection_observation(ProviderSelectionObservation::new(
            account_selection_wait_ms,
            lease.capacity_snapshot(),
        ));
        // Standalone Provider 端点没有可证明的账号 owner；Search metadata 必须按
        // 跨账号输入收敛到当前 lease，不能沿用下游声明的账号或 installation identity。
        let turn_metadata = request.turn_metadata.as_deref().and_then(|metadata| {
            crate::transport::request::scope_turn_metadata(metadata, lease.installation_id(), true)
        });
        let events = cold_json_response_stream(ColdJsonResponse {
            client: self.client.clone(),
            response_origin: request.response_origin,
            endpoint_path: request.endpoint_path,
            body: request.body,
            image_turn_id: request.image_turn_id,
            turn_metadata,
            context,
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            lease: Arc::clone(&lease),
            output_started_at: Instant::now(),
        });
        let stream = ProviderStream::new(metadata, events, lease);
        Ok(if allows_account_state_mutation {
            stream.with_filtered_account_feedback(
                Arc::clone(&self.account_feedback),
                openai_failure_affects_account_score,
            )
        } else {
            stream
        })
    }
}

struct RawJsonEndpointRequest {
    response_origin: Url,
    endpoint_path: &'static str,
    body: Bytes,
    image_turn_id: Option<String>,
    turn_metadata: Option<String>,
}

pub(super) struct ColdResponse {
    pub(super) client: CodexBackendClient,
    pub(super) response_origin: Url,
    pub(super) request: CodexResponsesRequest,
    pub(super) upstream_model: UpstreamModelId,
    pub(super) transport_policy: CodexProviderTransport,
    pub(super) context: AttemptContext,
    pub(super) selector: Arc<CodexCredentialSelector>,
    pub(super) quota: Arc<CodexCredentialQuotaService>,
    pub(super) catalog: Arc<CodexCredentialCatalogService>,
    pub(super) lease: Arc<CodexCredentialLease>,
    pub(super) output_started_at: Instant,
    pub(super) session_affinity_key: Option<ProviderSessionAffinityKey>,
    pub(super) session_affinity_key_hash: Option<String>,
    pub(super) session_transport_fallbacks: CodexSessionTransportFallbacks,
    pub(super) websocket_retry_count: u32,
    pub(super) stream_max_retries: u32,
    pub(super) session_capture: Option<OpenAiSessionCapture>,
}

pub(super) struct ColdJsonResponse {
    pub(super) client: CodexBackendClient,
    pub(super) response_origin: Url,
    pub(super) endpoint_path: &'static str,
    pub(super) body: Bytes,
    pub(super) image_turn_id: Option<String>,
    pub(super) turn_metadata: Option<String>,
    pub(super) context: AttemptContext,
    pub(super) selector: Arc<CodexCredentialSelector>,
    pub(super) quota: Arc<CodexCredentialQuotaService>,
    pub(super) lease: Arc<CodexCredentialLease>,
    pub(super) output_started_at: Instant,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct OpenAiSessionState {
    pub(super) account_id: String,
    pub(super) conversation_id: Option<String>,
    #[serde(default)]
    pub(super) turn_state: Option<String>,
    #[serde(default)]
    pub(super) client_turn_id: Option<String>,
    pub(super) continuation_scope: OpenAiContinuationScope,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OpenAiContinuationScope {
    Persisted,
    ConnectionLocal,
    ReplayRequired,
}

pub(super) struct OpenAiSessionCapture {
    pub(super) account_id: String,
    pub(super) conversation_id: Option<String>,
    pub(super) turn_state: Option<String>,
    pub(super) client_turn_id: Option<String>,
    pub(super) response_store: bool,
    pub(super) continuation_scope: Option<OpenAiContinuationScope>,
}

pub(super) fn same_client_turn(previous: Option<&str>, current: Option<&str>) -> bool {
    previous
        .zip(current)
        .is_some_and(|(previous, current)| !previous.is_empty() && previous == current)
}

pub(super) fn decode_openai_session_state(request: &GenerateRequest) -> Option<OpenAiSessionState> {
    request
        .provider_session_state(PROVIDER_NAME)
        .and_then(|state| serde_json::from_value(Value::Object(state.payload().clone())).ok())
}

pub(super) fn encode_openai_session_state(
    state: OpenAiSessionState,
) -> Result<ProviderSessionState, ProviderError> {
    let Value::Object(payload) = serde_json::to_value(state)
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?
    else {
        return Err(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        ));
    };
    ProviderSessionState::new(PROVIDER_NAME, payload)
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))
}

pub(super) fn attach_openai_session_update(
    events: &mut [ProviderEvent],
    capture: &mut Option<OpenAiSessionCapture>,
) {
    let Some(terminal_index) = events
        .iter()
        .position(|event| terminal_response_output(event).is_some())
    else {
        return;
    };
    let Some(capture) = capture.take() else {
        return;
    };
    let Some(continuation_scope) = capture.continuation_scope else {
        return;
    };
    let Ok(update) = encode_openai_session_state(OpenAiSessionState {
        account_id: capture.account_id,
        conversation_id: capture.conversation_id,
        turn_state: capture.turn_state,
        client_turn_id: capture.client_turn_id,
        continuation_scope,
    }) else {
        return;
    };
    events[terminal_index].attach_session_update(update);
}

pub(super) fn terminal_response_output(event: &ProviderEvent) -> Option<&[Value]> {
    let wire = event.wire_event()?;
    if wire.protocol() != PROVIDER_NAME {
        return None;
    }
    let event_type = wire
        .event_type()
        .or_else(|| wire.data().get("type").and_then(Value::as_str));
    matches!(
        event_type,
        Some("response.completed" | "response.incomplete")
    )
    .then(|| {
        wire.data()
            .pointer("/response/output")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
    })
    .flatten()
}

pub(super) enum CodexHandshakeAttemptError {
    Client(CodexClientError),
    Cancelled,
    Timeout,
}

pub(super) async fn create_response_attempt(
    client: &CodexBackendClient,
    request: &CodexResponsesRequest,
    request_context: CodexRequestContext<'_>,
    account_id: &str,
    deadline: SystemTime,
    cancellation: &CancellationToken,
) -> Result<CodexBackendStreamingResponse, CodexHandshakeAttemptError> {
    let Some(handshake_deadline) = remaining(deadline) else {
        return Err(CodexHandshakeAttemptError::Timeout);
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(handshake_deadline) => Err(CodexHandshakeAttemptError::Timeout),
        response = client.create_response_stream_with_deferred_websocket_recovery(
            request,
            request_context,
            Some(account_id),
        ) => response.map_err(CodexHandshakeAttemptError::Client),
    }
}

pub(super) fn map_handshake_attempt_error(
    error: CodexHandshakeAttemptError,
) -> MappedProviderFailure {
    match error {
        CodexHandshakeAttemptError::Client(error) => map_handshake_error(error),
        CodexHandshakeAttemptError::Cancelled => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Ambiguous,
        )),
        CodexHandshakeAttemptError::Timeout => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Ambiguous,
        )),
    }
}

pub(super) async fn create_json_attempt(
    request: &ColdJsonResponse,
    account: &ProviderAccount,
    installation_id: &str,
    authorization: &SecretString,
    cookie_header: Option<&SecretString>,
    account_selection: CodexAccountSelectionTelemetry<'_>,
) -> Result<CodexBackendJsonResponse, CodexHandshakeAttemptError> {
    let Some(handshake_deadline) = remaining(request.context.deadline()) else {
        return Err(CodexHandshakeAttemptError::Timeout);
    };
    let request_id = request.context.request_id().as_str();
    let mut request_context = CodexRequestContext::auxiliary(
        authorization.expose_secret(),
        account.upstream_account_id(),
        request_id,
        Some(installation_id),
    );
    request_context.cookie_header = cookie_header.map(ExposeSecret::expose_secret);
    request_context.turn_metadata = request.turn_metadata.as_deref();
    request_context.account_selection = account_selection;
    tokio::select! {
        biased;
        _ = request.context.cancellation().cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(handshake_deadline) => Err(CodexHandshakeAttemptError::Timeout),
        response = request.client.post_raw_json(
            request.endpoint_path,
            request.body.clone(),
            request.image_turn_id.as_deref(),
            request_context,
        ) => response.map_err(CodexHandshakeAttemptError::Client),
    }
}

pub(super) fn cold_json_response_stream(request: ColdJsonResponse) -> EventStream {
    Box::pin(async_stream::try_stream! {
        let allows_account_state_mutation = request.lease.allows_account_state_mutation();
        let failure_context = OpenAiFailureContext {
            client: &request.client,
            selector: &request.selector,
            quota: &request.quota,
            response_origin: &request.response_origin,
            cyber_policy_scope: None,
            allows_account_state_mutation,
        };
        let active_account = request.lease.account().clone();
        let cookie_header = build_cookie_header(request.lease.cookies())?;
        let authorization = request
            .lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::NotSent,
                )
            })?;
        let account_selection = CodexAccountSelectionTelemetry::new(
            request.lease.affinity_hit(),
            request.lease.escape_reason(),
            request.lease.account_switch(),
        );
        let response = create_json_attempt(
            &request,
            &active_account,
            request.lease.installation_id(),
            &authorization,
            cookie_header.as_ref(),
            account_selection,
        )
        .await;
        if let Err(CodexHandshakeAttemptError::Client(error)) = &response {
            log_client_upstream_error(
                UpstreamErrorLogContext::new(&request.context, &active_account, None),
                error,
            );
        }
        let response = match response.map_err(map_handshake_attempt_error) {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(&failure_context, &active_account, &failure).await;
                Err(failure.error)?;
                return;
            }
        };

        let mut metrics = response.transport_metrics.clone();
        metrics.first_event_ms = Some(
            i64::try_from(request.output_started_at.elapsed().as_millis()).unwrap_or(i64::MAX),
        );
        if let Some(observation) = codex_response_observation(
            CodexBackendTransport::HttpJson,
            &response.diagnostics,
            &response.response_metadata,
            &metrics,
            None,
            openai_response_timings(&metrics, &response.response_metadata),
        ) {
            yield ProviderEvent::observation(observation);
        }
        if allows_account_state_mutation {
            synchronize_passive_quota_headers(
                &request.quota,
                &active_account,
                &response.rate_limit_headers,
            )
            .await;
            if !response.set_cookie_headers.is_empty()
                && let Err(error) = request
                    .selector
                    .capture_response_cookies(
                        &active_account,
                        &request.response_origin,
                        &response.set_cookie_headers,
                    )
                    .await
            {
                tracing::warn!(
                    account_id = %active_account.id(),
                    error = %error,
                    "Failed to persist OpenAI provider endpoint response cookies"
                );
            }
        }

        let response_meta =
            ResponseMeta::for_provider_endpoint(request.context.request_id().as_str());
        yield ProviderEvent::canonical(GatewayEvent::Started(response_meta.clone()));
        let wire = ProtocolWireEvent::raw_json(PROVIDER_NAME, response.body).map_err(|_| {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        })?;
        yield ProviderEvent::wire(wire);
        yield ProviderEvent::canonical(GatewayEvent::Completed(
            response_meta.with_finish_reason(FinishReason::Stop),
        ));
    })
}

pub(super) fn cold_response_stream(response: ColdResponse) -> EventStream {
    let ColdResponse {
        client,
        response_origin,
        request,
        upstream_model,
        transport_policy,
        context,
        selector,
        quota,
        catalog,
        lease,
        output_started_at,
        session_affinity_key,
        session_affinity_key_hash,
        session_transport_fallbacks,
        websocket_retry_count,
        stream_max_retries,
        mut session_capture,
    } = response;
    Box::pin(async_stream::try_stream! {
        let cyber_policy_scope = lease.cyber_policy_scope().cloned();
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let failure_context = OpenAiFailureContext {
            client: &client,
            selector: &selector,
            quota: &quota,
            response_origin: &response_origin,
            cyber_policy_scope: cyber_policy_scope.as_ref(),
            allows_account_state_mutation,
        };
        let mut active_account = lease.account().clone();
        let cookie_header = build_cookie_header(lease.cookies())?;
        let authorization = lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::NotSent,
                )
            })?;
        let request_id = context.request_id().as_str().to_owned();
        let cancellation = context.cancellation().clone();
        let account_selection = CodexAccountSelectionTelemetry::new(
            lease.affinity_hit(),
            lease.escape_reason(),
            lease.account_switch(),
        );
        let request_transport_requirement = transport_requirement(&request);
        let response = create_response_attempt(
            &client,
            &request,
            codex_request_context(
                &request,
                &request_id,
                &active_account,
                lease.installation_id(),
                &authorization,
                cookie_header.as_ref(),
                account_selection,
            ),
            active_account.id().as_str(),
            context.deadline(),
            &cancellation,
        )
        .await;
        let websocket_failure_policy = match &response {
            Err(CodexHandshakeAttemptError::Client(error))
                if transport_policy == CodexProviderTransport::PreferWebSocket =>
            {
                websocket_client_failure_policy(error)
            }
            _ => None,
        };
        if let Err(CodexHandshakeAttemptError::Client(error)) = &response {
            log_client_upstream_error(
                UpstreamErrorLogContext::new(&context, &active_account, None),
                error,
            );
        }
        let response = response.map_err(map_handshake_attempt_error);
        let response = match response {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(policy) = websocket_failure_policy {
                    apply_websocket_recovery_policy(
                        &mut failure,
                        WebSocketRecoveryContext {
                            policy,
                            requirement: request_transport_requirement,
                            retry_count: websocket_retry_count,
                            max_retries: stream_max_retries,
                            request_id: context.request_id().as_str(),
                            attempt_index: context.attempt_index().get(),
                            account_id: active_account.id().as_str(),
                            session_affinity_key: session_affinity_key.as_ref(),
                            session_affinity_key_hash: session_affinity_key_hash.as_deref(),
                            session_transport_fallbacks: &session_transport_fallbacks,
                        },
                    );
                }
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(&failure_context, &active_account, &failure)
                .await;
                Err(failure.error)?;
                return;
            }
        };
        if !accepts_backend_transport(transport_policy, response.transport) {
            let failure = MappedProviderFailure::plain(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ));
            apply_failure(&failure_context, &active_account, &failure)
            .await;
            Err(failure.error)?;
            return;
        }
        if let Some(capture) = session_capture.as_mut() {
            capture.continuation_scope = Some(if capture.response_store {
                OpenAiContinuationScope::Persisted
            } else if response.transport == CodexBackendTransport::WebSocket
                && response.connection_local_continuation
            {
                OpenAiContinuationScope::ConnectionLocal
            } else {
                OpenAiContinuationScope::ReplayRequired
            });
            capture.turn_state = response.turn_state.clone().or(capture.turn_state.clone());
        }
        let mut observation_state = OpenAiResponseObservationState::from_backend_response(
            &response,
            &request,
            active_account.id().as_str(),
            context.attempt_index().get(),
        );
        if let Some(observation) = observation_state.observation() {
            yield ProviderEvent::observation(observation);
        }
        if let Some(etag) = response.response_metadata.models_etag.as_deref()
            && let Err(error) = catalog.observe_response_etag(etag)
        {
            tracing::warn!(
                error = %error,
                "OpenAI model ETag observation was rejected"
            );
        }
        if allows_account_state_mutation
            && !response.set_cookie_headers.is_empty()
            && let Ok(outcome) = selector
                .capture_response_cookies(
                    &active_account,
                    &response_origin,
                    &response.set_cookie_headers,
                )
                .await
                && let Some(revision) = outcome.credential_revision
                && let Ok(current) = selector.current_account(active_account.id()).await
                && current.revision().get() == revision
        {
            active_account = current;
        }
        let response_transport = response.transport;
        let websocket_connection_id = response.websocket_connection_id;
        let mut body = response.body;
        let failure_diagnostics = response.diagnostics.clone();
        let failure_set_cookie_headers = response.set_cookie_headers.clone();
        let failure_rate_limit_headers = response.rate_limit_headers.clone();
        let mut passive_quota_observation =
            OpenAiPassiveQuotaObservation::new(response.rate_limit_headers);
        let rate_limit_updates = response.rate_limit_updates;
        let turn_state_updates = response.turn_state_update;
        // OpenAI 线路为透明代理：HTTP SSE 与 WebSocket 两条上游均启用 raw 透传，
        // 下游按字节转发上游原文，避免 serde 往返改写数值/精度（大整数→f64、logprobs 等）。
        // WS 帧由 reducer 以 encode_sse_event(&event, raw) 逐字节内嵌上游原始 JSON
        // （transport/protocol/websocket.rs），push_frames 抽出的 data 即上游原文。
        let mut decoder = CodexCanonicalDecoder::new(upstream_model.as_str())
            .with_requested_service_tier(request.service_tier())
            .with_request_tool_pricing(upstream_model.as_str(), request.tools())
            .with_raw_sse_passthrough();
        let mut pre_commit_events = PreCommitClientEvents::new();
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                if allows_account_state_mutation {
                    synchronize_passive_quota(
                        &quota,
                        &active_account,
                        passive_quota_observation.rate_limits(),
                    )
                    .await;
                }
                Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent))?;
                return;
            };
            let replay_grace_deadline = pre_commit_events.replay_grace_deadline();
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(MappedProviderFailure::plain(provider_error(
                    ProviderErrorKind::Cancelled,
                    UpstreamSendState::Sent,
                ))),
                _ = tokio::time::sleep(stream_deadline) => Err(MappedProviderFailure::plain(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                ))),
                _ = wait_for_replay_grace(replay_grace_deadline) => Ok(PreCommitPoll::GraceElapsed),
                chunk = body.next() => match chunk {
                    Some(Ok(chunk)) => Ok(PreCommitPoll::Upstream(Some(chunk))),
                    Some(Err(error)) => {
                        log_client_upstream_error(
                            UpstreamErrorLogContext::new(
                                &context,
                                &active_account,
                                websocket_connection_id,
                            ),
                            &error,
                        );
                        Err(map_stream_error(error))
                    }
                    None => Ok(PreCommitPoll::Upstream(None)),
                },
            };
            let next = match next {
                Ok(PreCommitPoll::Upstream(next)) => next,
                Ok(PreCommitPoll::GraceElapsed) => {
                    for event in pre_commit_events.commit_pending() {
                        yield event;
                    }
                    continue;
                }
                Err(mut failure) => {
                    if failure.websocket_transport_retryable
                        && response_transport == CodexBackendTransport::WebSocket
                        && !pre_commit_events.is_committed()
                    {
                        apply_websocket_recovery_policy(
                            &mut failure,
                            WebSocketRecoveryContext {
                                policy: WebSocketFailurePolicy::Budgeted,
                                requirement: request_transport_requirement,
                                retry_count: websocket_retry_count,
                                max_retries: stream_max_retries,
                                request_id: context.request_id().as_str(),
                                attempt_index: context.attempt_index().get(),
                                account_id: active_account.id().as_str(),
                                session_affinity_key: session_affinity_key.as_ref(),
                                session_affinity_key_hash: session_affinity_key_hash.as_deref(),
                                session_transport_fallbacks: &session_transport_fallbacks,
                            },
                        );
                    }
                    let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
                    if !updates.is_empty() {
                        passive_quota_observation.observe(&updates);
                        let update_headers = rate_limit_update_headers(&updates);
                        if observation_state.merge_rate_limit_headers(&update_headers)
                            && let Some(observation) = observation_state.observation()
                        {
                            yield ProviderEvent::observation(observation);
                        }
                    }
                    if allows_account_state_mutation {
                        synchronize_passive_quota(
                            &quota,
                            &active_account,
                            passive_quota_observation.rate_limits(),
                        )
                        .await;
                    }
                    apply_failure(&failure_context, &active_account, &failure)
                    .await;
                    Err(failure.error)?;
                    return;
                }
            };
            let Some(chunk) = next else { break; };
            let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
            let rate_limits_changed = if updates.is_empty() {
                false
            } else {
                passive_quota_observation.observe(&updates);
                observation_state.merge_rate_limit_headers(&rate_limit_update_headers(&updates))
            };
            let turn_state_changed = if let Some(updates) = turn_state_updates.as_ref()
                && let Some(turn_state) = updates.lock().await.take()
            {
                if let Some(capture) = session_capture.as_mut() {
                    capture.turn_state = Some(turn_state.clone());
                }
                observation_state.merge_client_header("x-codex-turn-state", &turn_state)
            } else {
                false
            };
            let first_event_changed =
                observation_state.observe_stream_chunk(&chunk, output_started_at);
            let chunk_len = chunk.len();
            let (mut events, canonical_failure) = match decoder.push(&chunk) {
                CodexCanonicalOutcome::Events(events) => (events, None),
                CodexCanonicalOutcome::Failed(failure) => {
                    let (events, error, semantic_output_seen) = failure.into_parts();
                    (events, Some((error, semantic_output_seen)))
                }
            };
            pre_commit_events.observe_chunk(chunk_len);
            let service_tier_changed = observation_state
                .observe_upstream_service_tier(decoder.response_service_tier());
            let terminal_failure = canonical_failure.map(|(error, semantic_output_seen)| {
                log_canonical_upstream_error(
                    UpstreamErrorLogContext::new(
                        &context,
                        &active_account,
                        websocket_connection_id,
                    ),
                    response_transport,
                    &error,
                );
                let atomic_upstream_failure = matches!(&error, CodexCanonicalError::Upstream(_));
                (
                    map_canonical_error(
                        error,
                        &failure_diagnostics,
                        &failure_set_cookie_headers,
                        &failure_rate_limit_headers,
                        ReplayBoundary::from_semantic_output(
                            semantic_output_seen || pre_commit_events.is_committed(),
                        ),
                    ),
                    atomic_upstream_failure,
                )
            });
            let timing_signals = decoder.take_timing_signals();
            let timing_changed = first_event_changed
                || observation_state
                    .observe_timing_signals(timing_signals, output_started_at);
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            let terminal_changed = completed
                && observation_state.mark_completed(terminal_response_is_incomplete(&events));
            if allows_account_state_mutation && (completed || terminal_failure.is_some()) {
                synchronize_passive_quota(
                    &quota,
                    &active_account,
                    passive_quota_observation.rate_limits(),
                )
                .await;
            }
            if let Some((failure, _)) = terminal_failure.as_ref() {
                apply_failure(&failure_context, &active_account, failure)
                .await;
            }
            attach_openai_session_update(&mut events, &mut session_capture);
            if allows_account_state_mutation && completed && terminal_failure.is_none() {
                // 完成事件一旦交给下游，Core 可以立刻停止轮询 Provider stream；
                // 在此之前持久化亲和关系，保证成功请求不会因流被提前 drop 而丢失绑定。
                selector
                    .record_success(
                        &active_account,
                        session_affinity_key.as_ref(),
                        lease.affinity_expected_account_id(),
                    )
                    .await;
                selector
                    .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                    .await;
            }
            if (rate_limits_changed
                || service_tier_changed
                || timing_changed
                || turn_state_changed
                || terminal_changed)
                && let Some(observation) = observation_state.observation()
            {
                yield ProviderEvent::observation(observation);
            }
            if let Some((mut failure, atomic_upstream_failure)) = terminal_failure {
                let failure_after_commit =
                    timing_signals.semantic_output || pre_commit_events.is_committed();
                if failure_after_commit {
                    for event in pre_commit_events.commit(events) {
                        yield event;
                    }
                } else if atomic_upstream_failure {
                    failure.error = failure
                        .error
                        .with_atomic_client_events(pre_commit_events.take_for_failure(events));
                }
                Err(failure.error)?;
                return;
            }
            let events = pre_commit_events.stage(events, timing_signals, completed);
            for event in events {
                yield event;
            }
            if completed {
                return;
            }
        }
        let (mut events, canonical_failure) = match decoder.finish() {
            CodexCanonicalOutcome::Events(events) => (events, None),
            CodexCanonicalOutcome::Failed(failure) => {
                let (events, error, semantic_output_seen) = failure.into_parts();
                (events, Some((error, semantic_output_seen)))
            }
        };
        let terminal_failure = canonical_failure.map(|(error, semantic_output_seen)| {
            log_canonical_upstream_error(
                UpstreamErrorLogContext::new(
                    &context,
                    &active_account,
                    websocket_connection_id,
                ),
                response_transport,
                &error,
            );
            let atomic_upstream_failure = matches!(&error, CodexCanonicalError::Upstream(_));
            (
                map_canonical_error(
                    error,
                    &failure_diagnostics,
                    &failure_set_cookie_headers,
                    &failure_rate_limit_headers,
                    ReplayBoundary::from_semantic_output(
                        semantic_output_seen || pre_commit_events.is_committed(),
                    ),
                ),
                atomic_upstream_failure,
            )
        });
        let timing_signals = decoder.take_timing_signals();
        let service_tier_changed = observation_state
            .observe_upstream_service_tier(decoder.response_service_tier());
        let timing_changed = observation_state
            .observe_timing_signals(timing_signals, output_started_at);
        let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
        let rate_limits_changed = if updates.is_empty() {
            false
        } else {
            passive_quota_observation.observe(&updates);
            observation_state.merge_rate_limit_headers(&rate_limit_update_headers(&updates))
        };
        if allows_account_state_mutation {
            synchronize_passive_quota(
                &quota,
                &active_account,
                passive_quota_observation.rate_limits(),
            )
            .await;
        }
        if let Some((failure, _)) = terminal_failure.as_ref() {
            apply_failure(&failure_context, &active_account, failure)
            .await;
        }
        let turn_state_changed = if let Some(updates) = turn_state_updates.as_ref()
            && let Some(turn_state) = updates.lock().await.take()
        {
            if let Some(capture) = session_capture.as_mut() {
                capture.turn_state = Some(turn_state.clone());
            }
            observation_state.merge_client_header("x-codex-turn-state", &turn_state)
        } else {
            false
        };
        attach_openai_session_update(&mut events, &mut session_capture);
        let completed = events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        let terminal_changed = completed
            && observation_state.mark_completed(terminal_response_is_incomplete(&events));
        if allows_account_state_mutation && completed && terminal_failure.is_none() {
            // 同上：尾部 finish() 也可能产出 completed，亲和记录必须先于任何下游 yield。
            selector
                .record_success(
                    &active_account,
                    session_affinity_key.as_ref(),
                    lease.affinity_expected_account_id(),
                )
                .await;
            selector
                .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                .await;
        }
        if (service_tier_changed
            || timing_changed
            || rate_limits_changed
            || turn_state_changed
            || terminal_changed)
            && let Some(observation) = observation_state.observation()
        {
            yield ProviderEvent::observation(observation);
        }
        if let Some((mut failure, atomic_upstream_failure)) = terminal_failure {
            let failure_after_commit =
                timing_signals.semantic_output || pre_commit_events.is_committed();
            if failure_after_commit {
                for event in pre_commit_events.commit(events) {
                    yield event;
                }
            } else if atomic_upstream_failure {
                failure.error = failure
                    .error
                    .with_atomic_client_events(pre_commit_events.take_for_failure(events));
            }
            Err(failure.error)?;
            return;
        }
        let events = pre_commit_events.finish(events, timing_signals, completed);
        for event in events {
            yield event;
        }
    })
}
