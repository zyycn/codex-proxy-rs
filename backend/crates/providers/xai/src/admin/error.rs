//! Provider 错误到管理合同的唯一映射

use crate::credential::{
    FailureClass, GrokCredentialCatalogError, GrokCredentialRefreshError,
    GrokCredentialRepositoryError, GrokQuotaError, OAuthError,
};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::error::StoreErrorKind;
use gateway_core::provider_ports::{ProviderStoreError, ProviderStoreErrorKind};

pub(super) fn provider_error(kind: ProviderAdminErrorKind) -> ProviderAdminError {
    ProviderAdminError::new(kind)
}
pub(super) fn map_failure_class(class: FailureClass) -> ProviderAdminError {
    use ProviderAdminErrorKind as Kind;
    let (kind, message) = match class {
        FailureClass::Transient => (Kind::Unavailable, "授权服务暂不可用"),
        FailureClass::Ambiguous => (Kind::Ambiguous, "授权结果未知，核对账号状态前请勿重试"),
        FailureClass::CredentialPermanent => (Kind::Invalid, "凭据被拒绝，请检查账号授权"),
        FailureClass::ConfigurationPermanent => (Kind::Invalid, "授权配置无效，请检查客户端配置"),
        FailureClass::UserActionRequired => (Kind::Invalid, "请重新授权并确认"),
        FailureClass::Security => (Kind::Invalid, "授权校验失败，请核对来源和账号"),
        FailureClass::Unsupported => (Kind::Unsupported, "不支持此授权操作"),
    };
    provider_error(kind).with_public_message(message)
}

pub(super) fn map_oauth_error(error: OAuthError) -> ProviderAdminError {
    map_failure_class(error.class())
}

pub(super) fn map_provider_store_error(error: ProviderStoreError) -> ProviderAdminError {
    provider_error(match error.kind() {
        ProviderStoreErrorKind::InvalidData => ProviderAdminErrorKind::Invalid,
        ProviderStoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        ProviderStoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
    })
}
pub(super) fn map_store_error(error: gateway_core::error::StoreError) -> ProviderAdminError {
    provider_error(match error.kind() {
        StoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        StoreErrorKind::InvalidData | StoreErrorKind::InvalidState => {
            ProviderAdminErrorKind::NotFound
        }
        StoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
        _ => ProviderAdminErrorKind::Internal,
    })
    .with_source(error)
}

pub(super) fn map_repository_error(error: GrokCredentialRepositoryError) -> ProviderAdminError {
    use GrokCredentialRepositoryError as Error;
    provider_error(match &error {
        Error::InvalidInput(_)
        | Error::WrongProviderKind
        | Error::IdentityRebind
        | Error::InvalidCredentialData(_) => ProviderAdminErrorKind::Invalid,
        Error::CredentialNotFound => ProviderAdminErrorKind::NotFound,
        Error::StaleCredentialRevision(_) | Error::Conflict(_) | Error::RevisionOverflow => {
            ProviderAdminErrorKind::Conflict
        }
        Error::Store(_) => ProviderAdminErrorKind::Unavailable,
    })
    .with_source(error)
}

pub(super) fn map_refresh_error(error: GrokCredentialRefreshError) -> ProviderAdminError {
    use crate::GrokRefreshFailure as Failure;
    use GrokCredentialRefreshError as Error;
    use ProviderAdminErrorKind as Kind;
    let (kind, message) = match error {
        Error::Repository(error) => return map_repository_error(error),
        Error::Lease(error) => return map_provider_store_error(error),
        Error::LeaseBusy => (Kind::Conflict, "令牌刷新繁忙，请稍后重试"),
        Error::InvalidRefreshResponse => (Kind::Ambiguous, "新凭据无效，核对账号状态前请勿重试"),
        Error::Preparation => (Kind::Unavailable, "令牌刷新服务尚未就绪"),
        Error::ManualFailure(failure) => match failure {
            Failure::Transient => (Kind::Unavailable, "无法连接令牌刷新服务，请检查出站连接"),
            Failure::Ambiguous => (Kind::Ambiguous, "刷新结果未知，核对账号状态前请勿重试"),
            Failure::InvalidGrant => (Kind::Invalid, "刷新令牌已失效，请重新授权"),
            Failure::Banned => (Kind::Invalid, "上游账号已停用"),
            Failure::Rejected => (Kind::BadGateway, "令牌刷新失败"),
        },
    };
    provider_error(kind).with_public_message(message)
}

pub(super) fn map_quota_error(error: GrokQuotaError) -> ProviderAdminError {
    use GrokQuotaError as Error;
    use ProviderAdminErrorKind as Kind;
    let (kind, message) = match error {
        Error::AccountUnavailable => (Kind::NotFound, "当前账号无法查询额度"),
        Error::StaleCredentialSnapshot => (Kind::Conflict, "凭据已更新，请刷新账号列表"),
        Error::InvalidData => (Kind::Invalid, "凭据或额度数据无效"),
        Error::Upstream => (Kind::Unavailable, "额度查询失败"),
        Error::Store => (Kind::Unavailable, "额度查询暂不可用"),
    };
    provider_error(kind).with_public_message(message)
}

pub(super) fn map_catalog_error(error: GrokCredentialCatalogError) -> ProviderAdminError {
    use GrokCredentialCatalogError as Error;
    use ProviderAdminErrorKind as Kind;
    let (kind, message) = match error {
        Error::InvalidCredentialData => (Kind::Invalid, "查询模型的凭据无效，请检查账号授权"),
        Error::ConflictingModelFacts => (Kind::Invalid, "模型目录不一致，请重新查询"),
        Error::NoEligibleCredential => (Kind::NotFound, "没有可查询模型的账号"),
        Error::StaleCredentialSnapshot => (Kind::Conflict, "凭据已更新，请刷新账号列表"),
        Error::Upstream => (Kind::Unavailable, "模型查询失败"),
        Error::Cache | Error::Store => (Kind::Unavailable, "模型查询暂不可用"),
    };
    provider_error(kind).with_public_message(message)
}
