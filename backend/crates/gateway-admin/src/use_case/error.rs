//! 管理端口错误到用例错误的统一分类与来源保留

use crate::model::{AdminError, AdminErrorKind};
use crate::ports::store::{AdminStoreError, AdminStoreErrorKind};

pub(super) fn map_store_error(error: AdminStoreError, resource: &'static str) -> AdminError {
    let kind = match error.kind() {
        AdminStoreErrorKind::Invalid => AdminErrorKind::Invalid,
        AdminStoreErrorKind::NotFound => AdminErrorKind::NotFound,
        AdminStoreErrorKind::StaleRevision
        | AdminStoreErrorKind::DuplicateName
        | AdminStoreErrorKind::Conflict => AdminErrorKind::Conflict,
        AdminStoreErrorKind::Unavailable => AdminErrorKind::Unavailable,
    };
    let message = match kind {
        AdminErrorKind::Invalid => "请求参数不合法",
        AdminErrorKind::NotFound => "请求的资源不存在",
        AdminErrorKind::Conflict => "当前资源状态冲突，请刷新后重试",
        AdminErrorKind::Unavailable => "依赖服务暂不可用",
        _ => "服务内部错误",
    };
    AdminError::new(kind, message).with_source(OperationFailure {
        operation: resource,
        source: error.into(),
    })
}

pub(super) fn map_provider_error(
    error: crate::ports::provider::ProviderAdminError,
    resource: &'static str,
) -> AdminError {
    use crate::ports::provider::ProviderAdminErrorKind;

    let kind = match error.kind() {
        ProviderAdminErrorKind::Invalid => AdminErrorKind::Invalid,
        ProviderAdminErrorKind::Unsupported => AdminErrorKind::Invalid,
        ProviderAdminErrorKind::NotFound => AdminErrorKind::NotFound,
        ProviderAdminErrorKind::Conflict => AdminErrorKind::Conflict,
        ProviderAdminErrorKind::RateLimited => AdminErrorKind::RateLimited,
        ProviderAdminErrorKind::Ambiguous => AdminErrorKind::UpstreamResultUnknown,
        ProviderAdminErrorKind::Unavailable => AdminErrorKind::Unavailable,
        ProviderAdminErrorKind::CredentialRefreshRequired => AdminErrorKind::Unavailable,
        ProviderAdminErrorKind::BadGateway => AdminErrorKind::BadGateway,
        ProviderAdminErrorKind::Internal => AdminErrorKind::Internal,
    };
    let message = match kind {
        AdminErrorKind::Invalid => "Provider 请求不合法",
        AdminErrorKind::NotFound => "Provider 资源不存在",
        AdminErrorKind::Conflict => "Provider 资源状态冲突，请刷新后重试",
        AdminErrorKind::BadGateway => "上游服务请求失败",
        AdminErrorKind::UpstreamResultUnknown => "上游执行结果未知，请刷新状态后再决定是否重试",
        AdminErrorKind::Unavailable => "Provider 服务暂不可用",
        AdminErrorKind::Internal => "服务内部错误",
        _ => "Provider 操作失败",
    };
    AdminError::new(kind, error.public_message().unwrap_or(message)).with_source(OperationFailure {
        operation: resource,
        source: error.into(),
    })
}

// 用例的操作名补充端口资源上下文，底层分类与原始原因仍由来源持有
#[derive(Debug, thiserror::Error)]
#[error("admin operation failed: {operation}")]
struct OperationFailure {
    operation: &'static str,
    source: gateway_core::error::ErrorSource,
}
