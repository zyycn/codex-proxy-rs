//! xAI inference/compaction 流执行与终态校验。

use super::*;

pub(super) struct GrokStreamAttempt {
    pub(super) client_identity: GrokClientIdentity,
    pub(super) wire_profile: XaiWireProfileState,
    pub(super) credential_recovery: Arc<dyn GrokCredentialRecovery>,
    pub(super) responses_url: Url,
    pub(super) request: GrokResponsesRequest,
    pub(super) upstream_model: UpstreamModelId,
    pub(super) context: AttemptContext,
    pub(super) session: Arc<SelectedGrokSession>,
    pub(super) output_started_at: Instant,
    pub(super) session_capture: Option<GrokSessionCapture>,
    pub(super) reasoning_replay_capture: Option<GrokReasoningReplayCapture>,
}

pub(super) struct GrokCompactionStreamAttempt {
    pub(super) client_identity: GrokClientIdentity,
    pub(super) wire_profile: XaiWireProfileState,
    pub(super) credential_recovery: Arc<dyn GrokCredentialRecovery>,
    pub(super) responses_url: Url,
    pub(super) request: GrokCompactionRequest,
    pub(super) upstream_model: UpstreamModelId,
    pub(super) upstream_session_id: Option<String>,
    pub(super) context: AttemptContext,
    pub(super) session: Arc<SelectedGrokSession>,
    pub(super) reasoning_replay: GrokReasoningReplay,
    pub(super) reasoning_replay_key: Option<GrokReasoningReplayKey>,
}

pub(super) struct AcceptedGrokInference {
    response: GrokInferenceResponse,
    observation: ProviderResponseObservation,
}

pub(super) async fn next_grok_chunk(
    body: &mut GrokInferenceChunkStream,
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    upstream_model: &UpstreamModelId,
    context: &AttemptContext,
) -> Result<Option<bytes::Bytes>, ProviderError> {
    let Some(stream_deadline) = remaining(context.deadline()) else {
        return Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        ));
    };
    let cancellation = context.cancellation().clone();
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Sent,
        )),
        _ = tokio::time::sleep(stream_deadline) => Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        )),
        chunk = body.next() => match chunk {
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(error)) => Err(map_and_record_stream_transport_failure(
                selector,
                session,
                error,
                upstream_model,
            ).await),
            None => Ok(None),
        },
    }
}

pub(super) fn cold_compaction_http_sse_stream(
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    attempt: GrokCompactionStreamAttempt,
) -> EventStream {
    let GrokCompactionStreamAttempt {
        client_identity,
        wire_profile,
        credential_recovery,
        responses_url,
        mut request,
        upstream_model,
        upstream_session_id,
        context,
        session,
        reasoning_replay,
        reasoning_replay_key,
    } = attempt;
    Box::pin(async_stream::try_stream! {
        let headers = build_grok_headers(
            &wire_profile,
            &session,
            &client_identity,
            context.request_id(),
            upstream_session_id.as_deref(),
            None,
            &upstream_model,
        );
        let mut invalid_encrypted_content_retried = false;
        let accepted = loop {
            if context.cancellation().is_cancelled() {
                Err(provider_error(
                    ProviderErrorKind::Cancelled,
                    UpstreamSendState::NotSent,
                ))?;
                return;
            }
            let body = request.to_json_bytes().map_err(map_request_error)?;
            let inference_request = GrokInferenceRequest::new(
                responses_url.clone(),
                headers.clone(),
                body,
                session.binding().clone(),
            );
            let Some(handshake_deadline) = remaining(context.deadline()) else {
                Err(mark_transient_compaction_failure(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::NotSent,
                )))?;
                return;
            };
            let cancellation = context.cancellation().clone();
            let boundary = tokio::select! {
                biased;
                _ = cancellation.cancelled() => InferenceBoundary::Cancelled,
                _ = tokio::time::sleep(handshake_deadline) => InferenceBoundary::Deadline,
                response = transport.execute(inference_request) => InferenceBoundary::Response(response),
            };
            match boundary {
                InferenceBoundary::Cancelled => {
                    Err(provider_error(
                        ProviderErrorKind::Cancelled,
                        UpstreamSendState::Ambiguous,
                    ))?;
                    return;
                }
                InferenceBoundary::Deadline => {
                    Err(mark_transient_compaction_failure(provider_error(
                        ProviderErrorKind::Timeout,
                        UpstreamSendState::Ambiguous,
                    )))?;
                    return;
                }
                InferenceBoundary::Response(Ok(response)) => {
                    let observation = xai_response_observation(&response)
                        .map_err(mark_transient_compaction_failure)?;
                    break AcceptedGrokInference {
                        response,
                        observation,
                    };
                }
                InferenceBoundary::Response(Err(error)) => {
                    if !invalid_encrypted_content_retried
                        && is_invalid_encrypted_content_failure(&error)
                        && request.strip_invalid_encrypted_reasoning()
                    {
                        invalid_encrypted_content_retried = true;
                        continue;
                    }
                    let observation = xai_error_observation(&error).ok();
                    let credential_failure =
                        transport_credential_failure(&error, &upstream_model);
                    let error = map_continuation_failure(
                        &context,
                        map_transport_error_for_context(error, &context),
                    );
                    let error = recover_or_record_failure(
                        selector.as_ref(),
                        credential_recovery.as_ref(),
                        &session,
                        error,
                        credential_failure,
                        context.credential_recovery_attempted(),
                    )
                    .await;
                    if let Some(observation) = observation {
                        yield ProviderEvent::observation(observation);
                    }
                    Err(mark_transient_compaction_failure(error))?;
                    return;
                }
            }
        };
        yield ProviderEvent::observation(accepted.observation);

        let mut body = accepted.response.into_body();
        let mut canonical = GrokCanonicalDecoder::new(upstream_model.as_str());
        let mut summary = GrokCompactionSummaryDecoder::new();
        let mut facts = CompactionFacts::default();

        'stream: while let Some(chunk) = next_grok_chunk(
            &mut body,
            selector.as_ref(),
            &session,
            &upstream_model,
            &context,
        )
        .await
        .map_err(mark_transient_compaction_failure)?
        {
            let events = match canonical.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    let error = map_continuation_failure(&context, error);
                    let error = record_stream_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    )
                    .await;
                    Err(mark_transient_compaction_failure(error))?;
                    return;
                }
            };
            for event in events {
                summary.observe(&event).map_err(map_compaction_decode_error)?;
                facts.observe(&event);
                if facts.completed.is_some() {
                    break 'stream;
                }
            }
        }

        if facts.completed.is_none() {
            let events = match canonical.finish_without_terminal() {
                Ok(events) => events,
                Err(error) => {
                    let error = map_continuation_failure(&context, error);
                    let error = record_stream_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    )
                    .await;
                    Err(mark_transient_compaction_failure(error))?;
                    return;
                }
            };
            for event in events {
                summary.observe(&event).map_err(map_compaction_decode_error)?;
                facts.observe(&event);
                if facts.completed.is_some() {
                    break;
                }
            }
        }

        let started = facts
            .started
            .ok_or_else(|| mark_transient_compaction_failure(protocol_sent()))?;
        let completed = facts
            .completed
            .ok_or_else(|| mark_transient_compaction_failure(protocol_sent()))?;
        let (summary, encrypted_content) = summary
            .finish_with_encrypted_content()
            .map_err(map_compaction_decode_error)?;
        let (created, output_done, terminal) = crate::transport::compaction::compaction_wire_events(
            &started,
            &completed,
            summary.as_deref(),
            &encrypted_content,
            facts.created_response.as_ref(),
            facts.terminal_response.as_ref(),
        )
        .map_err(|_| mark_transient_compaction_failure(protocol_sent()))?
        .into_parts();
        ensure_sent_context(&context)?;
        if session.allows_account_state_mutation() {
            selector.record_success(&session).await;
        }
        if let Some(key) = reasoning_replay_key.as_ref() {
            reasoning_replay.clear(key);
        }
        yield ProviderEvent::canonical_with_wire(vec![GatewayEvent::Started(started)], created);
        yield ProviderEvent::wire(output_done);
        let mut terminal_facts = facts.metering;
        terminal_facts.push(GatewayEvent::Completed(completed));
        yield ProviderEvent::canonical_with_wire(terminal_facts, terminal);
    })
}

#[derive(Default)]
pub(super) struct CompactionFacts {
    started: Option<ResponseMeta>,
    completed: Option<ResponseMeta>,
    metering: Vec<GatewayEvent>,
    created_response: Option<Value>,
    terminal_response: Option<Value>,
}

impl CompactionFacts {
    fn observe(&mut self, event: &ProviderEvent) {
        self.capture_wire_response(event);
        for fact in event.canonical_facts() {
            match fact {
                GatewayEvent::Started(meta) if self.started.is_none() => {
                    self.started = Some(meta.clone());
                }
                GatewayEvent::Completed(meta) if self.completed.is_none() => {
                    self.completed = Some(meta.clone());
                }
                GatewayEvent::Usage(_)
                | GatewayEvent::CalculatedCost(_)
                | GatewayEvent::ProviderCost(_) => self.metering.push(fact.clone()),
                _ => {}
            }
        }
    }

    fn capture_wire_response(&mut self, event: &ProviderEvent) {
        let Some(wire) = event.wire_event().filter(|wire| wire.has_json_data()) else {
            return;
        };
        let event_type = wire
            .event_type()
            .or_else(|| wire.data().get("type").and_then(Value::as_str));
        let response = wire.data().get("response").cloned();
        match event_type {
            Some("response.created" | "response.in_progress")
                if self.created_response.is_none() =>
            {
                self.created_response = response;
            }
            Some("response.completed" | "response.incomplete")
                if self.terminal_response.is_none() =>
            {
                self.terminal_response = response;
            }
            _ => {}
        }
    }
}

pub(super) fn map_compaction_decode_error(error: GrokCompactionDecodeError) -> ProviderError {
    match error {
        GrokCompactionDecodeError::MissingEncryptedContent => {
            mark_transient_compaction_failure(protocol_sent())
        }
    }
}

pub(super) fn mark_transient_compaction_failure(error: ProviderError) -> ProviderError {
    if matches!(
        error.kind(),
        ProviderErrorKind::RateLimited
            | ProviderErrorKind::Timeout
            | ProviderErrorKind::Transport
            | ProviderErrorKind::Protocol
            | ProviderErrorKind::Unavailable
    ) {
        error.with_pre_delivery_retry()
    } else {
        error
    }
}

pub(super) fn cold_http_sse_stream(
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    attempt: GrokStreamAttempt,
) -> EventStream {
    let GrokStreamAttempt {
        client_identity,
        wire_profile,
        credential_recovery,
        responses_url,
        mut request,
        upstream_model,
        context,
        session,
        output_started_at,
        mut session_capture,
        mut reasoning_replay_capture,
    } = attempt;
    Box::pin(async_stream::try_stream! {
        if context.cancellation().is_cancelled() {
            Err(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))?;
        }
        let headers = build_grok_headers(
            &wire_profile,
            &session,
            &client_identity,
            context.request_id(),
            request.session_id(),
            None,
            &upstream_model,
        );
        let cancellation = context.cancellation().clone();
        let mut invalid_encrypted_content_retried = false;
        let response = loop {
            let body = request.to_json_bytes().map_err(map_request_error)?;
            let inference_request = GrokInferenceRequest::new(
                responses_url.clone(),
                headers.clone(),
                body,
                session.binding().clone(),
            );
            let Some(handshake_deadline) = remaining(context.deadline()) else {
                Err(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::NotSent,
                ))?;
                return;
            };
            let boundary = tokio::select! {
                biased;
                _ = cancellation.cancelled() => InferenceBoundary::Cancelled,
                _ = tokio::time::sleep(handshake_deadline) => InferenceBoundary::Deadline,
                response = transport.execute(inference_request) => InferenceBoundary::Response(response),
            };
            match boundary {
                InferenceBoundary::Cancelled => {
                    Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::Ambiguous))?;
                    return;
                }
                InferenceBoundary::Deadline => {
                    Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Ambiguous))?;
                    return;
                }
                InferenceBoundary::Response(Ok(response)) => break response,
                InferenceBoundary::Response(Err(error)) => {
                    if !invalid_encrypted_content_retried
                        && is_invalid_encrypted_content_failure(&error)
                        && request.strip_invalid_encrypted_reasoning()
                    {
                        invalid_encrypted_content_retried = true;
                        if let Some(capture) = session_capture.as_mut() {
                            capture.request_input = request.input_items();
                        }
                        continue;
                    }
                    let observation = xai_error_observation(&error)?;
                    let credential_failure = transport_credential_failure(&error, &upstream_model);
                    let error = map_continuation_failure(
                        &context,
                        map_transport_error_for_context(error, &context),
                    );
                    let error = recover_or_record_failure(
                        selector.as_ref(),
                        credential_recovery.as_ref(),
                        &session,
                        error,
                        credential_failure,
                        context.credential_recovery_attempted(),
                    )
                    .await;
                    yield ProviderEvent::observation(observation);
                    Err(error)?;
                    return;
                }
            }
        };

        let observation = xai_response_observation(&response)?;
        let base_timings = observation.timings();
        let mut first_token_ms: Option<u64> = None;
        yield ProviderEvent::observation(observation.clone());

        let mut body = response.into_body();
        let mut decoder = GrokCanonicalDecoder::for_request(upstream_model.as_str(), &request);
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                Err(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                ))?;
                return;
            };
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(provider_error(
                    ProviderErrorKind::Cancelled,
                    UpstreamSendState::Sent,
                )),
                _ = tokio::time::sleep(stream_deadline) => Err(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                )),
                chunk = body.next() => match chunk {
                    Some(Ok(chunk)) => Ok(Some(chunk)),
                    Some(Err(error)) => Err(map_and_record_stream_transport_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    ).await),
                    None => Ok(None),
                },
            }?;
            let Some(chunk) = next else {
                break;
            };
            let mut events = match decoder.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    let error = map_continuation_failure(&context, error);
                    let error = record_stream_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    )
                    .await;
                    Err(error)?;
                    return;
                }
            };
            // 首个非前导输出事件（结构帧也算）即上报携带 first_token_ms 的观测，
            // 供 Core 覆盖会话级兜底值。
            if decoder.take_output_start() && first_token_ms.is_none() {
                first_token_ms = Some(
                    u64::try_from(output_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
                yield ProviderEvent::observation(
                    observation
                        .clone()
                        .with_timings(ProviderResponseTimings {
                            first_token_ms,
                            ..base_timings
                        }),
                );
            }
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            attach_xai_session_update(&mut events, &mut session_capture)?;
            if let Some(capture) = reasoning_replay_capture.as_mut() {
                capture.observe(&events);
            }
            if completed && session.allows_account_state_mutation() {
                selector.record_success(&session).await;
            }
            for event in events {
                ensure_sent_context(&context)?;
                yield event;
            }
            if completed {
                return;
            }
        }
        let mut final_events = match decoder.finish() {
            Ok(events) => events,
            Err(error) => {
                let error = map_continuation_failure(&context, error);
                let error = record_stream_failure(
                    selector.as_ref(),
                    &session,
                    error,
                    &upstream_model,
                )
                .await;
                Err(error)?;
                return;
            }
        };
        let completed = final_events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        // 尾部 finish 补全缓冲中的首个输出帧时，同样上报首字。
        if decoder.take_output_start() && first_token_ms.is_none() {
            first_token_ms = Some(
                u64::try_from(output_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            );
            yield ProviderEvent::observation(
                observation
                    .clone()
                    .with_timings(ProviderResponseTimings {
                        first_token_ms,
                        ..base_timings
                    }),
            );
        }
        attach_xai_session_update(&mut final_events, &mut session_capture)?;
        if let Some(capture) = reasoning_replay_capture.as_mut() {
            capture.observe(&final_events);
        }
        if completed && session.allows_account_state_mutation() {
            selector.record_success(&session).await;
        }
        for event in final_events {
            ensure_sent_context(&context)?;
            yield event;
        }
    })
}
