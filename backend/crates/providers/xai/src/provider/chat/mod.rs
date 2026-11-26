//! Chat 直接访问 Grok Responses，复用传输与计量，不装配 Harness 会话转换

mod response;
mod tools;

use super::*;
use gateway_protocol::chat_completions::{CHAT_PROTOCOL, ChatIdentity};

impl GrokBuildProvider {
    pub(super) fn execute_selected_chat(
        &self,
        generate: GenerateRequest,
        middleware_headers: Vec<MiddlewareHeader>,
        attempt: SelectedGrokAttempt,
    ) -> Result<ProviderStream, ProviderError> {
        if generate.source_protocol() != CHAT_PROTOCOL
            || generate.provider_session_state(XAI_PROVIDER_NAME).is_some()
        {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let SelectedGrokAttempt {
            upstream_model,
            selected,
            account_selection_wait_ms,
            context,
            ..
        } = attempt;
        validate_selected_grok_account(&selected, &context, None)?;
        let request = GrokResponsesRequest::from_chat(
            &generate,
            upstream_model.as_str(),
            context.client_api_key_ref(),
        )
        .map_err(map_request_error)?;
        let original = generate.protocol_payload().body();
        let identity = ChatIdentity {
            id: format!("chatcmpl-{}", context.request_id().as_str()),
            created: SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model: original
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(upstream_model.as_str())
                .to_owned(),
        };
        let translator = response::Translator::new(identity, original);
        let metadata =
            provider_call_metadata(&upstream_model, &selected, account_selection_wait_ms)?;
        let selected = Arc::new(selected);
        let record_feedback = selected.allows_account_state_mutation();
        let events = cold_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.request_wire_profile(&context)?,
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request,
                middleware_headers,
                upstream_model,
                context,
                session: Arc::clone(&selected),
                session_capture: None,
            },
        );
        let stream = ProviderStream::new(metadata, events, selected)
            .with_native_response_translator(translator);
        Ok(if record_feedback {
            stream.with_account_feedback(Arc::clone(&self.account_feedback))
        } else {
            stream
        })
    }
}
