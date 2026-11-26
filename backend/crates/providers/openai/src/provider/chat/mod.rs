//! 选定账号后装配 Chat 与实际上游协议的直接转换

mod headers;
mod response;

use super::*;
use crate::credential::{ApiKeyProtocol, CodexRuntimeAuthentication};
use gateway_protocol::chat_completions::{ChatIdentity, messages, responses};
use std::time::{SystemTime, UNIX_EPOCH};

impl CodexProvider {
    pub(super) async fn execute_selected_chat(
        self: Arc<Self>,
        operation: Operation,
        mut headers: Vec<MiddlewareHeader>,
        model: UpstreamModelId,
        context: AttemptContext,
        mut selected: SelectedGenerate,
    ) -> Result<ProviderStream, ProviderError> {
        let Operation::Generate(generate) = &operation else {
            return Err(request_error());
        };
        if generate.source_protocol() != gateway_protocol::chat_completions::CHAT_PROTOCOL {
            return Err(request_error());
        }
        let original = generate.protocol_payload().body();
        let identity = ChatIdentity {
            id: format!("chatcmpl-{}", context.request_id().as_str()),
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            model: original
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(model.as_str())
                .to_owned(),
        };
        let mut source_context = generate.protocol_payload().context().clone();
        let target = selected.lease.authentication().api_protocol();
        let translator = response::ChatResponseTranslator::new(target, identity, original);
        match target {
            ApiKeyProtocol::Messages => {
                let capabilities = self
                    .catalog
                    .messages_model_capabilities(selected.lease.account(), &model)
                    .unwrap_or_default();
                let mut body = messages::convert_request(original, &capabilities)
                    .map_err(|error| request_error().with_source(error))?;
                body.insert("model".to_owned(), Value::String(model.as_str().to_owned()));
                headers::retain_messages_headers(&mut source_context, &mut headers);
                if body.get("speed").and_then(Value::as_str) == Some("fast") {
                    crate::compat::add_translated_fast_beta(
                        &mut headers,
                        &crate::transport::request::decode_passthrough_headers(&source_context),
                    );
                }
                let stream = self
                    .execute_selected_messages(
                        body,
                        source_context,
                        headers,
                        model,
                        context,
                        selected,
                    )
                    .await?;
                Ok(stream.with_native_response_translator(translator))
            }
            ApiKeyProtocol::Responses => {
                let mut body = responses::convert_request(original)
                    .map_err(|error| request_error().with_source(error))?;
                if matches!(
                    selected.lease.authentication(),
                    CodexRuntimeAuthentication::OAuth(_)
                ) {
                    crate::compat::normalize_translated_codex_service_tier(&mut body);
                }
                let body = serde_json::to_vec(&body).map_err(|_| request_error())?;
                let operation = operation
                    .translate_protocol_wire(OPENAI_PROTOCOL, body.into())
                    .map_err(|_| request_error())?;
                if let Operation::Generate(generate) = &operation {
                    selected.frozen_requirements = generate.wire_capability_requirements();
                }
                let stream = self
                    .execute_selected_generate(operation, headers, model, context, selected)
                    .await?;
                Ok(stream.with_native_response_translator(translator))
            }
        }
    }
}

fn request_error() -> ProviderError {
    provider_error(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
}

fn response_error() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
}
