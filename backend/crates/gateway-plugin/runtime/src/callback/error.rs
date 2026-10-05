//! 插件错误保留业务分类与原始详情；诊断日志仍由各 owner 单独生成

use gateway_core::{
    engine::{EngineError, execution::gateway_error_from_engine, middleware::MiddlewareError},
    error::{
        ContinuationFailure, ContinuationRecoveryDisposition, GatewayError, GatewayErrorKind,
        PreDeliveryRetry, ProviderError,
    },
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::{ErrorCode, PluginFault, SendState};
use serde_json::json;

pub(super) fn gateway(error: GatewayError) -> PluginFault {
    let code = match error.kind() {
        GatewayErrorKind::InvalidRequest | GatewayErrorKind::MessageTooBig => {
            ErrorCode::InvalidInput
        }
        GatewayErrorKind::Unsupported | GatewayErrorKind::ModelNotFound => ErrorCode::Unsupported,
        GatewayErrorKind::Unauthorized => ErrorCode::PermissionDenied,
        GatewayErrorKind::PolicyDenied => ErrorCode::Rejected,
        GatewayErrorKind::AccountCapacityUnavailable
        | GatewayErrorKind::ConcurrencyQueueFull
        | GatewayErrorKind::ConcurrencyQueueTimeout
        | GatewayErrorKind::RateLimited
        | GatewayErrorKind::NoAvailableProvider => ErrorCode::Capacity,
        GatewayErrorKind::Timeout => ErrorCode::Timeout,
        GatewayErrorKind::Cancelled => ErrorCode::Cancelled,
        GatewayErrorKind::UpstreamUnavailable
        | GatewayErrorKind::ProviderInfrastructureUnavailable => ErrorCode::Upstream,
        _ => ErrorCode::Fault,
    };
    let mut fault = PluginFault::new(code, error.client_message());
    fault.details = Some(json!({
        "kind": error.kind().as_str(),
        "type": error.client_error_type(),
        "code": error.client_error_code(),
        "retry_after_ms": error.retry_after().map(|value| value.as_millis()),
        "diagnostic": error.diagnostic().map(|value| json!({
            "message": value.as_str(), "stage": value.stage(), "code": value.code(),
        })),
    }));
    fault
}

fn provider(error: &ProviderError) -> PluginFault {
    let mut fault = gateway(GatewayError::from_provider(error));
    fault.send_state = match error.send_state() {
        UpstreamSendState::NotSent => SendState::NotSent,
        UpstreamSendState::Sent => SendState::Sent,
        UpstreamSendState::Ambiguous => SendState::Ambiguous,
    };
    fault.http_status = error.upstream_status();
    fault.details.get_or_insert_with(|| json!({}))["provider"] = json!({
        "kind": error.kind().as_str(),
        "code": error.upstream_code().map(|value| value.as_str()),
        "request_id": error.upstream_request_id().map(|value| value.as_str()),
        "retry_after_ms": error.retry_after().map(|value| value.as_millis()),
        "raw_upstream_error": error.raw_upstream_error().map(|value| value.as_str()),
        "response": error.client_visible_upstream_response().map(|value| json!({
            "status": value.status(), "content_type": value.content_type(),
            "headers": value.headers().iter().map(|header| json!({"name":header.name(), "value":header.value().as_ref()})).collect::<Vec<_>>(),
            "body": value.body().as_ref(),
        })),
        "connection": error.connection_observation().map(|value| json!({
            "connection_id": value.connection_id(), "exit_reason": value.exit_reason(),
            "age_ms": value.age_ms(), "idle_ms": value.idle_ms(),
        })),
        "continuation_unavailable_reason": error.continuation_unavailable_reason(),
        "continuation_failure": error.continuation_failure().map(|value| match value {
            ContinuationFailure::Busy => "busy",
            ContinuationFailure::HistoryUnavailable => "history_unavailable",
        }),
        "continuation_recovery_disposition": error.continuation_recovery_disposition().map(|value| match value {
            ContinuationRecoveryDisposition::RetryExactConnection => "retry_exact_connection",
            ContinuationRecoveryDisposition::ClientReplayRequired => "client_replay_required",
            ContinuationRecoveryDisposition::ProviderReplayAllowed => "provider_replay_allowed",
        }),
        "pre_delivery_retry": error.pre_delivery_retry().map(|value| match value {
            PreDeliveryRetry::AccountRotation => json!({"kind":"account_rotation"}),
            PreDeliveryRetry::SameAccountTransportFallback => json!({"kind":"same_account_transport_fallback"}),
            PreDeliveryRetry::SameAccountConnectionRetry {transport} => json!({"kind":"same_account_connection_retry","transport": match transport {
                gateway_core::engine::AttemptTransport::Default => json!({"kind":"default"}),
                gateway_core::engine::AttemptTransport::Retry(index) => json!({"kind":"retry","index":index.get()}),
                gateway_core::engine::AttemptTransport::Fallback => json!({"kind":"fallback"}),
            }}),
            PreDeliveryRetry::SameAccountTransportRetry {retry_index,delay} => json!({"kind":"same_account_transport_retry","retry_index":retry_index.get(),"delay_ms":delay.as_millis()}),
            PreDeliveryRetry::SameAccountTransientRetry {max_retries,initial_delay,max_delay} => json!({"kind":"same_account_transient_retry","max_retries":max_retries.get(),"initial_delay_ms":initial_delay.as_millis(),"max_delay_ms":max_delay.as_millis()}),
        }),
        "replay_safe": error.replay_is_safe(),
        "retry_same_account": error.retries_same_account(),
        "credential_recovery_required": error.requires_credential_recovery(),
        "sensitive_context_redacted": error.sensitive_context_was_redacted(),
        "has_atomic_client_events": error.has_atomic_client_events(),
    });
    fault
}

pub(super) fn engine(error: &EngineError) -> PluginFault {
    match error {
        EngineError::Provider(error) => provider(error),
        _ => {
            let mut fault = gateway(gateway_error_from_engine(error));
            fault.details.get_or_insert_with(|| json!({}))["engine"] =
                json!({"message":error.to_string()});
            fault
        }
    }
}

pub(super) fn middleware(error: &MiddlewareError) -> PluginFault {
    match error {
        MiddlewareError::Gateway(error) => gateway(error.clone()),
        MiddlewareError::Engine(error) => engine(error),
        MiddlewareError::Provider(error) => provider(error),
        MiddlewareError::Rejected => PluginFault::new(ErrorCode::Rejected, error.to_string()),
        MiddlewareError::InvalidState => PluginFault::new(ErrorCode::Conflict, error.to_string()),
        MiddlewareError::Fault => PluginFault::new(ErrorCode::Fault, error.to_string()),
        MiddlewareError::Remote { source: error, .. } => {
            match error.downcast_ref::<crate::RpcError>() {
                Some(error) => rpc(error),
                None => match error.downcast_ref::<std::sync::Arc<MiddlewareError>>() {
                    Some(error) => middleware(error),
                    None => PluginFault::new(ErrorCode::Fault, error.to_string()),
                },
            }
        }
    }
}

pub(crate) fn rpc(error: &crate::RpcError) -> PluginFault {
    use crate::RpcError;
    let code = match error {
        RpcError::Remote(fault) => return fault.clone(),
        RpcError::Timeout => ErrorCode::Timeout,
        RpcError::Cancelled => ErrorCode::Cancelled,
        RpcError::Capacity => ErrorCode::Capacity,
        _ => ErrorCode::Fault,
    };
    PluginFault::new(code, error.to_string())
}

pub(crate) fn rpc_middleware(error: crate::RpcError) -> MiddlewareError {
    let rejected =
        matches!(&error, crate::RpcError::Remote(fault) if fault.code == ErrorCode::Rejected);
    MiddlewareError::Remote {
        source: Box::new(error),
        rejected,
    }
}
