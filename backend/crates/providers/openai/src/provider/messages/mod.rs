//! Messages 原生执行与计数复用已选账号租约，不承担跨协议转换

mod count;
mod stream;

use super::*;
use crate::credential::{ApiKeyProtocol, CodexRuntimeAuthentication};
use gateway_protocol::anthropic::ANTHROPIC_PROTOCOL;
use serde_json::Map;

impl CodexProvider {
    pub(super) async fn execute_native_messages(
        self: Arc<Self>,
        operation: Operation,
        headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        mut selected: SelectedGenerate,
    ) -> Result<ProviderStream, ProviderError> {
        let Operation::Generate(generate) = operation else {
            return Err(invalid_request());
        };
        if generate.protocol_payload().protocol() != ANTHROPIC_PROTOCOL
            || selected.lease.authentication().api_protocol() != ApiKeyProtocol::Messages
        {
            return Err(invalid_request());
        }
        if !context.is_diagnostic_required_account() {
            let affinity = crate::credential::derive_messages_session_affinity(
                generate.protocol_payload(),
                &headers,
                context.client_api_key_ref(),
            );
            self.selector
                .validate_selected_affinity(
                    &mut selected.lease,
                    affinity.as_ref(),
                    None,
                    context.account_selection_policy(),
                )
                .await
                .map_err(map_selection_error)?;
        }
        let mut body = generate.protocol_payload().body().clone();
        body.insert("model".to_owned(), Value::String(model.as_str().to_owned()));
        self.execute_selected_messages(
            body,
            generate.protocol_payload().context().clone(),
            headers,
            model,
            context,
            selected,
        )
        .await
    }

    pub(super) async fn execute_selected_messages(
        self: Arc<Self>,
        body: Map<String, Value>,
        source_context: Map<String, Value>,
        headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        selected: SelectedGenerate,
    ) -> Result<ProviderStream, ProviderError> {
        let SelectedGenerate {
            lease,
            account_selection_wait_ms,
            ..
        } = selected;
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
            .client_for_request(&context, lease.account())?
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
            .with_account_privacy(
                context.privacy(),
                context.cancellation().clone(),
                lease.account(),
                None,
            );
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
