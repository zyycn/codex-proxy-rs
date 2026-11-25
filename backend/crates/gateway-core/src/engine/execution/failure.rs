//! 执行边界的错误分类与资源释放诊断

use super::contract::ClientAuthenticationError;
use crate::{
    engine::{EngineError, ModelRequestId},
    error::{GatewayError, GatewayErrorKind},
};

pub(super) fn authentication_gateway_error(error: ClientAuthenticationError) -> GatewayError {
    match error {
        ClientAuthenticationError::InvalidKey => {
            GatewayError::new(GatewayErrorKind::Unauthorized, "Client API key is invalid.")
        }
        ClientAuthenticationError::SnapshotUnavailable => GatewayError::new(
            GatewayErrorKind::Internal,
            "Gateway configuration is unavailable.",
        ),
        ClientAuthenticationError::ProviderUnavailable => GatewayError::new(
            GatewayErrorKind::Internal,
            "Client authentication service is unavailable.",
        ),
    }
}

pub(super) fn map_routing_error(error: crate::validation::RoutingError) -> GatewayError {
    match error {
        crate::validation::RoutingError::InvalidReasoningVariant => GatewayError::new(
            GatewayErrorKind::InvalidRequest,
            "A supported reasoning effort is required for this model group.",
        ),
        crate::validation::RoutingError::GroupModelDenied => GatewayError::new(
            GatewayErrorKind::PolicyDenied,
            crate::validation::GROUP_MODEL_DENIED_MESSAGE,
        )
        .with_client_code(crate::validation::GROUP_MODEL_DENIED_CODE),
        crate::validation::RoutingError::ModelNotFound {
            model,
            mapped_model,
        } => GatewayError::new(
            GatewayErrorKind::ModelNotFound,
            if model == mapped_model {
                "Model unavailable for this API key. Check the model name."
            } else {
                "Mapped model unavailable for this API key. Check the model mapping."
            },
        ),
        crate::validation::RoutingError::NoCapableProvider { .. }
        | crate::validation::RoutingError::NoCapableProviderEndpoint { .. }
        | crate::validation::RoutingError::EmptyAccountScope => GatewayError::new(
            GatewayErrorKind::NoAvailableProvider,
            "No provider can execute this request.",
        ),
        crate::validation::RoutingError::UnsupportedProviderEndpoint { .. } => GatewayError::new(
            GatewayErrorKind::Unsupported,
            "The selected provider does not support this operation.",
        ),
        _ => GatewayError::new(
            GatewayErrorKind::Internal,
            "Runtime routing configuration is invalid.",
        ),
    }
}

pub fn gateway_error_from_engine(error: &EngineError) -> GatewayError {
    match error {
        EngineError::Cancelled => {
            GatewayError::new(GatewayErrorKind::Cancelled, "Request was cancelled.")
        }
        EngineError::Deadline => {
            GatewayError::new(GatewayErrorKind::Timeout, "Request deadline elapsed.")
        }
        EngineError::Provider(provider) => GatewayError::from_provider(provider),
        EngineError::EmptyRoutingPlan | EngineError::ProviderNotRegistered { .. } => {
            GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "No provider is available.",
            )
        }
        EngineError::Store(source) => {
            GatewayError::new(GatewayErrorKind::Internal, "Request execution failed.")
                .with_source(source.clone())
        }
        _ => GatewayError::new(GatewayErrorKind::Internal, "Request execution failed."),
    }
}

pub(super) async fn record_resource_failure(
    diagnostics: &dyn crate::diagnostics::OperationalDiagnostics,
    request_id: &ModelRequestId,
    operation: &'static str,
    message: &'static str,
    source: impl Into<crate::error::ErrorSource>,
) {
    let mut failure = crate::diagnostics::OperationalFailure::new(
        "core",
        operation,
        "resource_unavailable",
        message,
    );
    failure.correlation_id = Some(request_id.as_str().to_owned());
    failure.details = crate::error::ErrorDetails::capture(Some(&source.into()), None, false);
    if diagnostics.record_failure(failure).await.is_err() {
        tracing::warn!(
            request_id = request_id.as_str(),
            operation,
            "resource diagnostic could not be recorded"
        );
    }
}
