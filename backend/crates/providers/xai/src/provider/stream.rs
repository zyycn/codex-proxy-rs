//! xAI 原生推理流执行、计量和终态校验

use super::*;

pub(super) struct GrokStreamAttempt {
    pub(super) client_identity: GrokClientIdentity,
    pub(super) wire_profile: XaiWireProfileState,
    pub(super) credential_recovery: Arc<dyn GrokCredentialRecovery>,
    pub(super) responses_url: Url,
    pub(super) request: GrokResponsesRequest,
    pub(super) middleware_headers: Vec<MiddlewareHeader>,
    pub(super) upstream_model: UpstreamModelId,
    pub(super) context: AttemptContext,
    pub(super) session: Arc<SelectedGrokSession>,
    pub(super) session_capture: Option<GrokSessionCapture>,
}

// xAI 的首字要求真实语义输出，结构事件只记首事件时间
fn observe_output_timings(
    observation: &mut ProviderResponseObservation,
    events: &[ProviderEvent],
    started_at: Instant,
) -> bool {
    if events.is_empty() {
        return false;
    }
    let previous = observation.timings();
    let mut timings = previous;
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    timings.first_event_ms.get_or_insert(elapsed_ms);
    for event in events.iter().flat_map(ProviderEvent::canonical_facts) {
        match event {
            GatewayEvent::ReasoningDelta(delta) if !delta.text.is_empty() => {
                timings.first_reasoning_ms.get_or_insert(elapsed_ms);
                timings.first_token_ms.get_or_insert(elapsed_ms);
            }
            GatewayEvent::TextDelta(delta) if !delta.text.is_empty() => {
                timings.first_text_ms.get_or_insert(elapsed_ms);
                timings.first_token_ms.get_or_insert(elapsed_ms);
            }
            GatewayEvent::ToolCallDelta(delta) if !delta.arguments_delta.is_empty() => {
                timings.first_token_ms.get_or_insert(elapsed_ms);
            }
            _ => {}
        }
    }
    if timings == previous {
        return false;
    }
    *observation = observation.clone().with_timings(timings);
    true
}

fn append_middleware_grok_headers(
    target: &mut Vec<crate::transport::GrokHeader>,
    headers: &[MiddlewareHeader],
) -> Result<(), ProviderError> {
    let mut replaced = std::collections::HashSet::new();
    for header in headers {
        if replaced.insert(header.name().to_ascii_lowercase()) {
            target.retain(|existing| !existing.name().eq_ignore_ascii_case(header.name()));
        }
        let value = std::str::from_utf8(header.value())
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        // 插件可写任意字段，值不进入 Debug；transport 仍读取完整原值
        target.push(crate::transport::GrokHeader::sensitive(
            header.name().to_owned(),
            crate::SecretValue::new(value),
        ));
    }
    Ok(())
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
        request,
        middleware_headers,
        upstream_model,
        context,
        session,
        mut session_capture,
    } = attempt;
    Box::pin(async_stream::try_stream! {
        if context.cancellation().is_cancelled() {
            Err(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))?;
        }
        let mut headers = build_grok_headers(
            &wire_profile,
            &session,
            &client_identity,
            context.request_id(),
            request.session_id(),
            None,
            &upstream_model,
        );
        append_middleware_grok_headers(&mut headers, &middleware_headers)?;
        let cancellation = context.cancellation().clone();
        let response = {
            let body = request.to_json_bytes().map_err(map_request_error)?;
            let inference_request = GrokInferenceRequest::new(
                responses_url,
                headers,
                body,
                session.binding().clone(),
            ).with_trace(context.trace());
            let inference_request = super::outbound::prepare_request(
                inference_request, &context, &session, &upstream_model,
            ).await?;
            if context.deadline().is_elapsed() {
                Err(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::NotSent,
                ))?;
                return;
            };
            let boundary = tokio::select! {
                biased;
                _ = cancellation.cancelled() => InferenceBoundary::Cancelled,
                _ = context.deadline().wait() => InferenceBoundary::Deadline,
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
                InferenceBoundary::Response(Ok(response)) => response,
                InferenceBoundary::Response(Err(error)) => {
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

        let mut observation = xai_response_observation(&response)?;
        yield ProviderEvent::observation(observation.clone());

        let mut body = response.into_body();
        let mut decoder = GrokCanonicalDecoder::for_request(upstream_model.as_str(), &request)
            .with_pricing(context.pricing().get("xai").and_then(|p| p.get(upstream_model.as_str())).cloned());
        loop {
            if context.deadline().is_elapsed() {
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
                _ = context.deadline().wait() => Err(provider_error(
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
            if observe_output_timings(&mut observation, &events, context.timing_started_at()) {
                yield ProviderEvent::observation(observation.clone());
            }
            if let Some(tier) = decoder.response_service_tier()
                && observation.service_tier() != Some(tier)
            {
                observation = observation.with_service_tier_if_valid(tier.to_owned());
                yield ProviderEvent::observation(observation.clone());
            }
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            attach_xai_session_update(&mut events, &mut session_capture)?;
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
        if observe_output_timings(&mut observation, &final_events, context.timing_started_at()) {
            yield ProviderEvent::observation(observation.clone());
        }
        let completed = final_events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        if let Some(tier) = decoder.response_service_tier()
            && observation.service_tier() != Some(tier)
        {
            observation = observation.with_service_tier_if_valid(tier.to_owned());
            yield ProviderEvent::observation(observation.clone());
        }
        attach_xai_session_update(&mut final_events, &mut session_capture)?;
        if completed && session.allows_account_state_mutation() {
            selector.record_success(&session).await;
        }
        for event in final_events {
            ensure_sent_context(&context)?;
            yield event;
        }
    })
}
