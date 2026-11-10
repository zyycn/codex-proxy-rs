//! 现有账号租约内按 API 协议分派；转换不改变账号选择和中间件执行顺序

mod count;
mod stream;
mod translator;

use super::*;
use crate::credential::{ApiKeyProtocol, CodexRuntimeAuthentication};
use gateway_protocol::anthropic::{
    ANTHROPIC_PROTOCOL, messages_to_responses_request, responses_to_messages_request,
};
use serde_json::Map;

impl CodexProvider {
    pub(super) async fn execute_selected_protocol_generate(
        self: Arc<Self>,
        operation: Operation,
        mut headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        mut selected: SelectedGenerate,
    ) -> Result<ProviderStream, ProviderError> {
        if operation.capability_requirements() != selected.frozen_requirements {
            return Err(invalid_request());
        }
        let Operation::Generate(generate) = &operation else {
            return Err(invalid_request());
        };
        let source = generate.protocol_payload().protocol();
        if !matches!(source, PROVIDER_NAME | ANTHROPIC_PROTOCOL) {
            return Err(invalid_request());
        }
        if selected.lease.authentication().api_protocol() == ApiKeyProtocol::Messages {
            if context.continuation_attempt() == ContinuationAttempt::Native
                || generate.provider_session_state(PROVIDER_NAME).is_some()
                || (source == PROVIDER_NAME
                    && generate
                        .protocol_payload()
                        .body()
                        .get("previous_response_id")
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty()))
            {
                return Err(continuation_replay_required_error("scope_unavailable"));
            }
            let original = generate.protocol_payload().body();
            let mut body = if source == ANTHROPIC_PROTOCOL {
                let mut body = original.clone();
                gateway_protocol::anthropic::strip_foreign_thinking_for_messages(&mut body);
                body
            } else {
                let capabilities = tokio::select! {
                    biased;
                    _ = context.cancellation().cancelled() => return Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)),
                    _ = context.deadline().wait() => return Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)),
                    result = self.catalog.messages_model_capabilities(selected.lease.account(), &model) => result.ok().flatten().unwrap_or_default(),
                };
                let mut body = responses_to_messages_request(original, &capabilities)
                    .map_err(|_| invalid_request())?;
                // Responses 的 HTTP JSON 和 SSE 共用事件输出，跨协议上游统一请求事件流
                body.insert("stream".to_owned(), Value::Bool(true));
                body
            };
            body.insert("model".to_owned(), Value::String(model.as_str().to_owned()));
            let mut source_context = generate.protocol_payload().context().clone();
            if source == PROVIDER_NAME {
                retain_target_headers(source, &mut source_context, &mut headers);
                if body.get("speed").and_then(Value::as_str) == Some("fast") {
                    crate::compat::add_translated_fast_beta(
                        &mut headers,
                        &crate::transport::request::decode_passthrough_headers(&source_context),
                    );
                }
            }
            let translator = (source == PROVIDER_NAME)
                .then(|| translator::MessagesResponseTranslator::new(original));
            drop(operation);
            let stream = self
                .execute_selected_messages(body, source_context, headers, model, context, selected)
                .await?;
            return Ok(if let Some(translator) = translator {
                stream.with_native_response_translator(translator)
            } else {
                stream
            });
        }
        if source == ANTHROPIC_PROTOCOL {
            let original = generate.protocol_payload().body();
            let mut source_context = generate.protocol_payload().context().clone();
            retain_target_headers(source, &mut source_context, &mut headers);
            let translator = translator::ResponsesResponseTranslator::new(original);
            let translated =
                messages_to_responses_request(original).map_err(|_| invalid_request())?;
            let body = serde_json::to_vec(&translated).map_err(|_| invalid_request())?;
            let operation = operation
                .replace_protocol_wire(None, source_context)
                .map_err(|_| invalid_request())?
                .translate_protocol_wire(PROVIDER_NAME, body.into())
                .map_err(|_| invalid_request())?;
            // 原始能力已在上面核对；Responses 终端只校验它实际消费的 wire 事实
            if let Operation::Generate(generate) = &operation {
                selected.frozen_requirements = generate.wire_capability_requirements();
            }
            let stream = self
                .execute_selected_generate(operation, headers, model, context, selected)
                .await?;
            return Ok(stream.with_native_response_translator(translator));
        }
        self.execute_selected_generate(operation, headers, model, context, selected)
            .await
    }

    async fn execute_selected_messages(
        self: Arc<Self>,
        body: Map<String, Value>,
        source_context: Map<String, Value>,
        headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        selected: SelectedGenerate,
    ) -> Result<ProviderStream, ProviderError> {
        let SelectedGenerate {
            mut lease,
            account_selection_wait_ms,
            ..
        } = selected;
        if !context.is_diagnostic_required_account() {
            self.selector
                .validate_translated_selection(
                    &mut lease,
                    None,
                    None,
                    context.account_selection_policy(),
                )
                .await
                .map_err(map_selection_error)?;
        }
        self.messages_stream(
            body,
            source_context,
            headers,
            model,
            context,
            lease,
            account_selection_wait_ms,
            None,
        )
    }

    #[expect(clippy::too_many_arguments)]
    fn messages_stream(
        &self,
        body: Map<String, Value>,
        source_context: Map<String, Value>,
        headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        lease: CodexCredentialLease,
        account_selection_wait_ms: u64,
        count_protocol: Option<&'static str>,
    ) -> Result<ProviderStream, ProviderError> {
        let CodexRuntimeAuthentication::ApiKey(authentication) = lease.authentication() else {
            return Err(invalid_request());
        };
        let provider = ProviderKind::new(PROVIDER_NAME).map_err(|_| invalid_request())?;
        let transport = if body.get("stream").and_then(Value::as_bool) == Some(true) {
            HTTP_SSE_TRANSPORT
        } else {
            HTTP_JSON_TRANSPORT
        };
        let metadata = ProviderCallMetadata::new(
            provider.clone(),
            model.clone(),
            lease.account_id().clone(),
            UpstreamTransport::new(transport).map_err(|_| invalid_request())?,
        )
        .with_credential_revision(lease.account().revision())
        .with_account_snapshot(Some(
            gateway_core::account::AccountHistorySnapshot::from_account(lease.account()),
        ))
        .with_selection_observation(ProviderSelectionObservation::new(
            account_selection_wait_ms,
            lease.capacity_snapshot(),
        ));
        let client = self
            .client_for_request(&context)?
            .for_account(lease.account())
            .map_err(|error| map_client_error(error, UpstreamSendState::NotSent, None).error)?
            .with_messages_api_base_url(&authentication.configuration.base_url)
            .with_connection_budget(context.connection_budget().clone())
            .with_middleware_headers(headers)
            .with_outbound_middleware(context.outbound_middleware(
                provider,
                Some(model.as_str().to_owned()),
                lease.account_id().clone(),
            ))
            .with_privacy(context.privacy(), context.cancellation().clone());
        let response_origin = Url::parse(&crate::transport::endpoint_url(
            &authentication.configuration.base_url,
            "/messages",
        ))
        .map_err(|_| invalid_request())?;
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let lease = Arc::new(lease);
        let events = stream::cold_messages_stream(stream::ColdMessages {
            client,
            fast_requested: body.get("speed").and_then(Value::as_str) == Some("fast"),
            body: serde_json::to_vec(&body)
                .map_err(|_| invalid_request())?
                .into(),
            passthrough_headers: crate::transport::request::decode_passthrough_headers(
                &source_context,
            ),
            context,
            lease: Arc::clone(&lease),
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            model,
            response_origin,
            count_protocol,
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

fn invalid_request() -> ProviderError {
    provider_error(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
}

fn protocol_error() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
}

fn retain_target_headers(
    source: &str,
    context: &mut Map<String, Value>,
    headers: &mut Vec<MiddlewareHeader>,
) {
    let keep = |name: &str| {
        let name = name.to_ascii_lowercase();
        if source == ANTHROPIC_PROTOCOL {
            !name.starts_with("anthropic-") && !name.starts_with("x-anthropic-")
        } else {
            ![
                "openai-",
                "x-openai-",
                "x-codex-",
                "x-oai-",
                "x-responsesapi-",
                "chatgpt-",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
                && !matches!(
                    name.as_str(),
                    "session-id" | "conversation-id" | "thread-id" | "turn-id" | "session_id"
                )
        }
    };
    if let Some(entries) = context
        .get_mut(crate::transport::request::PASSTHROUGH_HEADERS_CONTEXT_KEY)
        .and_then(Value::as_array_mut)
    {
        entries.retain(|entry| {
            entry
                .as_array()
                .and_then(|entry| entry.first())
                .and_then(Value::as_str)
                .is_some_and(keep)
        });
    }
    headers.retain(|header| keep(header.name()));
}
