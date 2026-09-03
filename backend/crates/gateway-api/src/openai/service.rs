//! OpenAI wire adapter 到 Core 执行用例的唯一映射。

use std::net::IpAddr;
use std::sync::Arc;

use gateway_core::engine::continuation::PreviousResponseId;
use gateway_core::engine::execution::{
    AuthenticatedClient, ClientAuthenticationError, ClientTransport, ExecutionRequestMetadata,
    ExecutionService, StartExecution, StartProviderExecution, StartedExecution,
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};
use gateway_core::routing::{PublicModelId, PublicModelProfile};
use uuid::Uuid;

use super::auth::ClientApiKeyAuthError;
use super::responses::{ContinuationIntent, DecodedResponsesRequest};

/// OpenAI HTTP/WS adapter 共享的 Core 与连接生命周期能力。
#[derive(Clone)]
pub(crate) struct OpenAiService {
    execution: Arc<dyn ExecutionService>,
    lifecycle: Arc<dyn ConnectionLifecycle>,
}

impl OpenAiService {
    #[must_use]
    pub(crate) const fn new(
        execution: Arc<dyn ExecutionService>,
        lifecycle: Arc<dyn ConnectionLifecycle>,
    ) -> Self {
        Self {
            execution,
            lifecycle,
        }
    }

    pub(crate) fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientApiKeyAuthError> {
        self.execution
            .authenticate(plaintext)
            .map_err(map_authentication_error)
    }

    pub(crate) fn public_models(&self, client: &AuthenticatedClient) -> Vec<String> {
        self.execution
            .public_models(client)
            .into_iter()
            .map(|model| model.as_str().to_owned())
            .collect()
    }

    pub(crate) fn public_model_profiles(
        &self,
        client: &AuthenticatedClient,
    ) -> Vec<PublicModelProfile> {
        self.execution.public_model_profiles(client)
    }

    pub(crate) fn contains_public_model(
        &self,
        client: &AuthenticatedClient,
        model: &PublicModelId,
    ) -> bool {
        self.execution.contains_public_model(client, model)
    }

    pub(crate) async fn start_response(
        &self,
        client: AuthenticatedClient,
        request: DecodedResponsesRequest,
        transport: ClientTransport,
        endpoint: &'static str,
    ) -> Result<StartedExecution, GatewayError> {
        let (operation, metadata) = request.into_parts();
        let public_model = PublicModelId::from_client_wire(metadata.requested_model().to_owned())
            .map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::ModelNotFound,
                "requested model was not found",
            )
        })?;
        let previous_response_id = match metadata.continuation() {
            ContinuationIntent::None => None,
            ContinuationIntent::PreviousResponseId(value) => {
                Some(PreviousResponseId::new(value.clone()))
            }
        };
        self.execution
            .start(StartExecution {
                client,
                public_model,
                operation,
                metadata: ExecutionRequestMetadata {
                    protocol: "openai".to_owned(),
                    endpoint: endpoint.to_owned(),
                    transport,
                    stream: metadata.stream(),
                    client_ip: metadata.client_ip(),
                    user_agent: metadata.user_agent().map(str::to_owned),
                    previous_response_id,
                },
            })
            .await
    }

    pub(crate) async fn start_provider_endpoint(
        &self,
        client: AuthenticatedClient,
        operation: gateway_core::operation::Operation,
        client_ip: Option<IpAddr>,
        user_agent: Option<String>,
        endpoint: &'static str,
    ) -> Result<StartedExecution, GatewayError> {
        let provider = gateway_core::routing::ProviderKind::new("openai").map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "OpenAI provider identifier is invalid",
            )
        })?;
        self.execution
            .start_provider_endpoint(StartProviderExecution {
                client,
                provider,
                operation,
                metadata: ExecutionRequestMetadata {
                    protocol: "openai".to_owned(),
                    endpoint: endpoint.to_owned(),
                    transport: ClientTransport::HttpJson,
                    stream: false,
                    client_ip,
                    user_agent,
                    previous_response_id: None,
                },
            })
            .await
    }

    pub(crate) fn try_register_connection(
        &self,
    ) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        self.lifecycle.try_register()
    }

    #[must_use]
    pub(crate) fn lifecycle(&self) -> Arc<dyn ConnectionLifecycle> {
        Arc::clone(&self.lifecycle)
    }

    #[must_use]
    pub(crate) fn next_request_id(&self) -> String {
        format!("req_{}", Uuid::now_v7().simple())
    }
}

const fn map_authentication_error(error: ClientAuthenticationError) -> ClientApiKeyAuthError {
    match error {
        ClientAuthenticationError::InvalidKey => ClientApiKeyAuthError::InvalidKey,
        ClientAuthenticationError::SnapshotUnavailable => ClientApiKeyAuthError::RuntimeUnavailable,
    }
}
