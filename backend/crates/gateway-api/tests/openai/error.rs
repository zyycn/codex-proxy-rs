use axum::http::StatusCode;
use gateway_core::engine::{EngineError, UpstreamSendState};
use gateway_core::error::{
    GatewayError, GatewayErrorKind, ProviderError, ProviderErrorKind, StoreError, StoreErrorKind,
};

use gateway_api::openai::error::{
    gateway_error_contract, gateway_error_from_engine, openai_error_response,
};

#[test]
fn invalid_request_error_should_map_to_openai_bad_request() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::InvalidRequest),
        (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid_request",
        )
    );
}

#[test]
fn unsupported_error_should_map_to_openai_bad_request() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::Unsupported),
        (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "unsupported_capability",
        )
    );
}

#[test]
fn model_not_found_error_should_map_to_openai_model_not_found() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::ModelNotFound),
        (
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "model_not_found",
        )
    );
}

#[test]
fn no_available_provider_error_should_map_to_service_unavailable() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::NoAvailableProvider),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "no_available_provider",
        )
    );
}

#[test]
fn rate_limited_error_should_map_to_openai_retryable_status() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::RateLimited),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate_limit_exceeded",
        )
    );
}

#[test]
fn upstream_unavailable_error_should_map_to_bad_gateway() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::UpstreamUnavailable),
        (
            StatusCode::BAD_GATEWAY,
            "server_error",
            "upstream_unavailable",
        )
    );
}

#[test]
fn timeout_error_should_map_to_gateway_timeout() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::Timeout),
        (
            StatusCode::GATEWAY_TIMEOUT,
            "server_error",
            "request_timeout",
        )
    );
}

#[test]
fn cancelled_error_should_not_be_misclassified_as_upstream_failure() {
    assert_eq!(
        gateway_error_contract(GatewayErrorKind::Cancelled),
        (
            StatusCode::REQUEST_TIMEOUT,
            "server_error",
            "request_cancelled",
        )
    );
}

#[test]
fn engine_provider_error_should_preserve_retry_classification() {
    let error = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::RateLimited,
        UpstreamSendState::Sent,
    ));

    assert_eq!(
        gateway_error_from_engine(&error).kind(),
        GatewayErrorKind::RateLimited
    );
}

#[test]
fn local_provider_capacity_exhaustion_should_map_to_service_unavailable() {
    let error = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::Unavailable,
        UpstreamSendState::NotSent,
    ));

    assert_eq!(
        gateway_error_from_engine(&error),
        GatewayError::new(
            GatewayErrorKind::NoAvailableProvider,
            "no upstream Provider instance or account is currently available"
        )
    );
}

#[test]
fn sent_provider_unavailability_should_remain_bad_gateway() {
    let error = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::Unavailable,
        UpstreamSendState::Sent,
    ));

    assert_eq!(
        gateway_error_from_engine(&error).kind(),
        GatewayErrorKind::UpstreamUnavailable
    );
}

#[test]
fn engine_provider_invalid_request_should_remain_a_client_request_error() {
    let error = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::Sent,
    ));

    assert_eq!(
        gateway_error_from_engine(&error).kind(),
        GatewayErrorKind::InvalidRequest
    );
}

#[test]
fn engine_provider_unsupported_capability_should_remain_unsupported() {
    let error = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::Unsupported,
        UpstreamSendState::NotSent,
    ));

    assert_eq!(
        gateway_error_from_engine(&error).kind(),
        GatewayErrorKind::Unsupported
    );
}

#[test]
fn engine_provider_credential_failures_should_not_impersonate_client_auth_failures() {
    for kind in [
        ProviderErrorKind::Unauthorized,
        ProviderErrorKind::PermissionDenied,
    ] {
        let error = EngineError::Provider(ProviderError::new(kind, UpstreamSendState::Sent));
        let mapped = gateway_error_from_engine(&error);

        assert_eq!(mapped.kind(), GatewayErrorKind::UpstreamUnavailable);
        assert_eq!(
            mapped.safe_message(),
            "upstream authentication resource is unavailable"
        );
    }
}

#[test]
fn engine_provider_quota_exhaustion_should_use_the_retryable_capacity_contract() {
    let error = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::QuotaExhausted,
        UpstreamSendState::Sent,
    ));

    assert_eq!(
        gateway_error_from_engine(&error).kind(),
        GatewayErrorKind::RateLimited
    );
}

#[test]
fn engine_provider_timeout_and_cancellation_should_remain_distinct() {
    let timeout = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::Timeout,
        UpstreamSendState::Ambiguous,
    ));
    let cancelled = EngineError::Provider(ProviderError::new(
        ProviderErrorKind::Cancelled,
        UpstreamSendState::Sent,
    ));

    assert_eq!(
        (
            gateway_error_from_engine(&timeout).kind(),
            gateway_error_from_engine(&cancelled).kind(),
        ),
        (GatewayErrorKind::Timeout, GatewayErrorKind::Cancelled)
    );
}

#[test]
fn engine_provider_runtime_failures_should_collapse_to_safe_unavailability() {
    for kind in [
        ProviderErrorKind::Transport,
        ProviderErrorKind::Protocol,
        ProviderErrorKind::Unavailable,
        ProviderErrorKind::ProcessTerminated,
    ] {
        let error = EngineError::Provider(ProviderError::new(kind, UpstreamSendState::Ambiguous));

        assert_eq!(
            gateway_error_from_engine(&error),
            GatewayError::new(
                GatewayErrorKind::UpstreamUnavailable,
                "upstream service is unavailable"
            )
        );
    }
}

#[test]
fn engine_store_error_should_collapse_to_safe_internal_error() {
    let error = EngineError::Store(StoreError::new(StoreErrorKind::Unavailable));

    assert_eq!(
        gateway_error_from_engine(&error),
        GatewayError::new(GatewayErrorKind::Internal, "gateway execution failed")
    );
}
#[test]
fn openai_error_response_should_preserve_only_safe_contract_fields() {
    let (status, body) = openai_error_response(
        StatusCode::BAD_GATEWAY,
        "upstream service is unavailable",
        "server_error",
        "upstream_unavailable",
    );

    assert_eq!(
        (status, body.0),
        (
            StatusCode::BAD_GATEWAY,
            serde_json::json!({
                "error": {
                    "message": "upstream service is unavailable",
                    "type": "server_error",
                    "code": "upstream_unavailable"
                }
            }),
        )
    );
}
