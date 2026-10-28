//! 插件上游适配的唯一错误投影边界，保留来源并生成安全分类

use std::time::Duration;

use crate::RpcError;
use gateway_core::{
    error::{
        ClientVisibleUpstreamError, OpaqueUpstreamValue, ProviderDiagnostic, ProviderError,
        ProviderErrorKind, RawUpstreamError,
    },
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::upstream_adapter::{UpstreamFailure, UpstreamFailureKind},
};

pub(super) fn rpc_error(error: RpcError, sent: UpstreamSendState) -> ProviderError {
    let (code, message) = error.diagnostic();
    let kind = match error {
        RpcError::Remote(fault) => return fault_error(fault, sent),
        RpcError::Timeout => ProviderErrorKind::Timeout,
        RpcError::Cancelled => ProviderErrorKind::Cancelled,
        _ => ProviderErrorKind::Unavailable,
    };
    ProviderError::new(kind, sent)
        .with_diagnostic(ProviderDiagnostic::new(message).with_classification("plugin_rpc", code))
        .with_source(error)
}

pub(super) fn fault_error(fault: PluginFault, sent: UpstreamSendState) -> ProviderError {
    let kind = match fault.code {
        ErrorCode::Timeout => ProviderErrorKind::Timeout,
        ErrorCode::Cancelled => ProviderErrorKind::Cancelled,
        ErrorCode::Unsupported => ProviderErrorKind::Unsupported,
        ErrorCode::InvalidInput | ErrorCode::Rejected | ErrorCode::PermissionDenied => {
            ProviderErrorKind::InvalidRequest
        }
        _ => ProviderErrorKind::Unavailable,
    };
    // 仅保存响应方向的插件 fault，普通日志与公共错误继续使用宿主分类
    let raw = serde_json::json!({"source": "plugin_upstream_adapter", "fault": fault}).to_string();
    ProviderError::new(kind, sent)
        .with_diagnostic(
            ProviderDiagnostic::new(format!(
                "Plugin upstream adapter returned a fault: {:?}",
                fault.code,
            ))
            .with_classification("plugin_rpc", "remote"),
        )
        .with_raw_upstream_error(RawUpstreamError::new(raw))
}

pub(super) fn failure_error(
    failure: UpstreamFailure,
    sent: UpstreamSendState,
) -> Result<ProviderError, ProviderError> {
    if failure.message.len() > 64 * 1024
        || failure.code.as_ref().is_some_and(|code| code.len() > 256)
        || failure
            .error_type
            .as_ref()
            .is_some_and(|value| value.len() > 256)
        || failure
            .status
            .is_some_and(|status| !(400..=599).contains(&status))
        || failure
            .retry_after_ms
            .is_some_and(|delay| delay > 24 * 60 * 60 * 1000)
    {
        return Err(invalid(sent));
    }
    let kind = match failure.kind {
        UpstreamFailureKind::InvalidRequest => ProviderErrorKind::InvalidRequest,
        UpstreamFailureKind::Unsupported => ProviderErrorKind::Unsupported,
        UpstreamFailureKind::Unauthorized => ProviderErrorKind::Unauthorized,
        UpstreamFailureKind::PermissionDenied => ProviderErrorKind::PermissionDenied,
        UpstreamFailureKind::RateLimited => ProviderErrorKind::RateLimited,
        UpstreamFailureKind::QuotaExhausted => ProviderErrorKind::QuotaExhausted,
        UpstreamFailureKind::Timeout => ProviderErrorKind::Timeout,
        UpstreamFailureKind::Unavailable => ProviderErrorKind::Unavailable,
        UpstreamFailureKind::Protocol => ProviderErrorKind::Protocol,
    };
    // 插件已经解析的上游错误与客户端投影分别承载，消费 message 前保存原始返回
    let raw =
        serde_json::to_string(&failure).map_err(|source| invalid(sent).with_source(source))?;
    let mut error = ProviderError::new(kind, sent)
        .with_upstream_failure_fields(gateway_core::upstream::UpstreamFailureFields {
            status: failure.status,
            code: failure.code.clone(),
            error_type: failure.error_type.clone(),
            message: Some(failure.message.clone()),
        })
        .with_raw_upstream_error(RawUpstreamError::new(raw))
        .with_diagnostic(
            ProviderDiagnostic::new(format!(
                "Plugin upstream failure: kind={}, status={:?}",
                kind.as_str(),
                failure.status,
            ))
            .with_classification("upstream", "plugin_upstream_failure"),
        );
    if let Some(code) = &failure.code {
        error = error.with_upstream_code(OpaqueUpstreamValue::new(code.clone()));
    }
    error = error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        failure.message,
        failure.code,
        failure.error_type,
    ));
    if let Some(status) = failure.status {
        error = error.with_status(status);
    }
    if let Some(delay) = failure.retry_after_ms {
        error = error.with_retry_after(Duration::from_millis(delay));
    }
    Ok(error)
}

pub(super) fn invalid(sent: UpstreamSendState) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, sent).with_diagnostic(
        ProviderDiagnostic::new("Plugin upstream adapter returned an invalid event")
            .with_classification("plugin_protocol", "invalid_event"),
    )
}
