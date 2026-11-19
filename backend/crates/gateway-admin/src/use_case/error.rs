//! 管理端口错误到用例错误的统一分类与来源保留

use crate::model::{AdminError, AdminErrorKind};
use crate::ports::store::{AdminStoreError, AdminStoreErrorKind};

pub(super) fn map_store_error(error: AdminStoreError, resource: &'static str) -> AdminError {
    let (kind, message) = match error.kind() {
        AdminStoreErrorKind::Invalid => (AdminErrorKind::Invalid, "请求参数不合法"),
        AdminStoreErrorKind::NotFound => (AdminErrorKind::NotFound, "请求的资源不存在"),
        AdminStoreErrorKind::StaleRevision => {
            (AdminErrorKind::Conflict, "数据已更新，请刷新后再操作")
        }
        AdminStoreErrorKind::DuplicateName => (AdminErrorKind::Conflict, "名称已存在"),
        AdminStoreErrorKind::Conflict => (AdminErrorKind::Conflict, "当前资源状态不允许此操作"),
        AdminStoreErrorKind::Unavailable => (AdminErrorKind::Unavailable, "存储服务暂不可用"),
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
        ProviderAdminErrorKind::Ambiguous => AdminErrorKind::UpstreamResultUnknown,
        ProviderAdminErrorKind::Unavailable => AdminErrorKind::Unavailable,
        ProviderAdminErrorKind::CredentialRefreshRequired => AdminErrorKind::Unavailable,
        ProviderAdminErrorKind::BadGateway => AdminErrorKind::BadGateway,
        ProviderAdminErrorKind::Internal => AdminErrorKind::Internal,
    };
    let message = match error.kind() {
        ProviderAdminErrorKind::Invalid => "服务商请求参数不合法",
        ProviderAdminErrorKind::Unsupported => "该服务商不支持此操作",
        ProviderAdminErrorKind::NotFound => "服务商资源不存在",
        ProviderAdminErrorKind::Conflict => "服务商资源状态冲突，请刷新后重试",
        ProviderAdminErrorKind::BadGateway => "上游服务请求失败",
        ProviderAdminErrorKind::Ambiguous => "上游执行结果未知，请刷新状态后再决定是否重试",
        ProviderAdminErrorKind::Unavailable => "服务商暂不可用",
        ProviderAdminErrorKind::CredentialRefreshRequired => "账号凭据需要刷新",
        ProviderAdminErrorKind::Internal => "服务内部错误",
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
