//! Provider 错误到管理合同的唯一映射

use crate::credential::{
    CodexCredentialAdminError, CodexCredentialCatalogError, CodexCredentialQuotaError,
    CodexOAuthAdminError, CodexProfileAvatarError, CodexProfileStatisticsError,
    CodexResetCreditsError,
};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::error::StoreErrorKind;

pub(super) fn provider_admin_error(kind: ProviderAdminErrorKind) -> ProviderAdminError {
    ProviderAdminError::new(kind)
}
pub(super) fn map_store_error(error: gateway_core::error::StoreError) -> ProviderAdminError {
    provider_admin_error(match error.kind() {
        StoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        StoreErrorKind::InvalidData | StoreErrorKind::InvalidState => {
            ProviderAdminErrorKind::NotFound
        }
        StoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
        _ => ProviderAdminErrorKind::Internal,
    })
    .with_source(error)
}

pub(super) fn map_credential_admin_error(error: CodexCredentialAdminError) -> ProviderAdminError {
    use crate::credential::token_client::PersonalAccessTokenError;
    use CodexCredentialAdminError as Error;
    use ProviderAdminErrorKind as Kind;
    let upstream_message = error.upstream_message().map(ToOwned::to_owned);
    let (kind, public_message) = match error {
        Error::PersonalAccessToken(error) => match error {
            PersonalAccessTokenError::InvalidToken => (
                Kind::Invalid,
                "请输入完整 PAT，以 at- 开头且不含空白或控制字符",
            ),
            PersonalAccessTokenError::Rejected => (Kind::Invalid, "PAT 被拒绝，请检查令牌和权限"),
            PersonalAccessTokenError::Unavailable => (Kind::Unavailable, "PAT 验证服务暂不可用"),
            PersonalAccessTokenError::InvalidResponse => (Kind::BadGateway, "PAT 身份信息无效"),
        },
        Error::InvalidInput => (Kind::Invalid, "账号数据格式无效"),
        Error::InvalidCredential => (Kind::Invalid, "凭据无效，请更正或重新授权"),
        Error::MissingRefreshToken => (Kind::Invalid, "账号没有刷新令牌，请重新授权"),
        Error::RefreshRejected { code, .. } => (
            Kind::Invalid,
            refresh_rejection_message(code.as_deref()).unwrap_or("刷新令牌已失效，请重新授权"),
        ),
        Error::AccountBanned { .. } => (Kind::Invalid, "上游账号已停用"),
        Error::NotFound => (Kind::NotFound, "账号不存在，请刷新列表"),
        Error::RefreshLeaseUnavailable => (Kind::Conflict, "令牌刷新繁忙，请稍后重试"),
        Error::RefreshUnavailable => (Kind::Unavailable, "令牌刷新服务暂不可用"),
        Error::RefreshUpstream { status, code, .. } => (
            Kind::BadGateway,
            refresh_rejection_message(code.as_deref()).unwrap_or(match status {
                401 | 403 => "令牌刷新被拒绝，请检查账号授权",
                429 => "令牌刷新过于频繁，请稍后重试",
                500..=599 => "令牌刷新服务异常，请稍后重试",
                _ => "令牌刷新失败",
            }),
        ),
        Error::RefreshAmbiguous { .. } => (Kind::Ambiguous, "刷新结果未知，核对账号状态前请勿重试"),
    };
    let error = provider_admin_error(kind).with_public_message(public_message);
    match upstream_message {
        Some(message) => error.with_message(message),
        None => error,
    }
}

fn refresh_rejection_message(code: Option<&str>) -> Option<&'static str> {
    // 与官方 Codex 的失败原因对齐；只解释管理提示，不改动 Worker 的终态/退避判定
    match code.map(str::to_ascii_lowercase).as_deref() {
        Some("refresh_token_expired") => Some("刷新令牌已过期，请重新授权"),
        Some("refresh_token_reused") => Some("刷新令牌已被使用，请重新授权"),
        Some("refresh_token_invalidated") => Some("刷新令牌已被撤销，请重新授权"),
        // token_expired 也用于 RT 校验失败，不据此断言具体到期原因
        Some("token_expired") => Some("刷新令牌不可用，请重新授权"),
        Some("invalid_grant") => Some("刷新令牌无效，请重新授权"),
        _ => None,
    }
}

pub(super) const fn credential_admin_error_code(error: &CodexCredentialAdminError) -> &'static str {
    match error {
        CodexCredentialAdminError::PersonalAccessToken(_) => {
            "personal_access_token_validation_failed"
        }
        CodexCredentialAdminError::InvalidInput => "invalid_input",
        CodexCredentialAdminError::InvalidCredential => "invalid_credential",
        CodexCredentialAdminError::NotFound => "not_found",
        CodexCredentialAdminError::MissingRefreshToken => "missing_refresh_token",
        CodexCredentialAdminError::RefreshLeaseUnavailable => "refresh_lease_unavailable",
        CodexCredentialAdminError::RefreshRejected { .. } => "refresh_rejected",
        CodexCredentialAdminError::AccountBanned { .. } => "account_banned",
        CodexCredentialAdminError::RefreshUnavailable => "refresh_unavailable",
        CodexCredentialAdminError::RefreshUpstream { .. } => "refresh_upstream_failed",
        CodexCredentialAdminError::RefreshAmbiguous { .. } => "refresh_ambiguous",
    }
}

pub(super) const fn provider_admin_error_code(kind: ProviderAdminErrorKind) -> &'static str {
    match kind {
        ProviderAdminErrorKind::Invalid => "invalid",
        ProviderAdminErrorKind::Unsupported => "unsupported",
        ProviderAdminErrorKind::NotFound => "not_found",
        ProviderAdminErrorKind::Conflict => "conflict",
        ProviderAdminErrorKind::Ambiguous => "ambiguous",
        ProviderAdminErrorKind::Unavailable => "unavailable",
        ProviderAdminErrorKind::CredentialRefreshRequired => "credential_refresh_required",
        ProviderAdminErrorKind::BadGateway => "bad_gateway",
        ProviderAdminErrorKind::Internal => "internal",
    }
}

pub(super) fn log_import_failure(stage: &'static str, error: &'static str) {
    tracing::warn!(
        import_stage = stage,
        import_error = error,
        "OpenAI credential import preparation failed"
    );
}

pub(super) fn map_oauth_error(error: CodexOAuthAdminError) -> ProviderAdminError {
    use crate::credential::token_client::AuthorizationCodeExchangeError as Exchange;
    use CodexOAuthAdminError as Error;
    use ProviderAdminErrorKind as Kind;
    let (kind, message) = match &error {
        Error::InvalidInput => (Kind::Invalid, "授权参数无效，请重新授权"),
        Error::CallbackRejected => (Kind::Invalid, "授权回调校验失败，请重新授权"),
        Error::TokenRejected | Error::Exchange(Exchange::Rejected) => {
            (Kind::Invalid, "授权码被拒绝，请重新授权")
        }
        Error::Credential => (Kind::Invalid, "授权返回的凭据无效"),
        Error::NotFound | Error::FlowExpired => (Kind::NotFound, "授权流程已失效，请重新授权"),
        Error::Conflict => (Kind::Conflict, "授权状态冲突，请核对账号状态"),
        Error::Exchange(Exchange::Ambiguous { .. }) => {
            (Kind::Ambiguous, "授权结果未知，核对账号状态前请勿重试")
        }
        Error::Exchange(Exchange::Connection {
            timed_out: true, ..
        }) => (Kind::Unavailable, "连接授权服务超时，请检查出站连接"),
        Error::Exchange(Exchange::Connection {
            timed_out: false, ..
        }) => (Kind::Unavailable, "无法连接授权服务，请检查出站连接"),
        Error::Exchange(Exchange::Unavailable) => (Kind::Unavailable, "授权服务暂不可用"),
        Error::StorageUnavailable => (Kind::Unavailable, "授权状态存储暂不可用"),
        Error::Profile(_) => (Kind::Unavailable, "授权客户端身份暂不可用"),
    };
    provider_admin_error(kind)
        .with_public_message(message)
        .with_source(error)
}

pub(super) const fn oauth_error_code(error: &CodexOAuthAdminError) -> &'static str {
    use crate::credential::token_client::AuthorizationCodeExchangeError as Exchange;
    match error {
        CodexOAuthAdminError::InvalidInput => "invalid_input",
        CodexOAuthAdminError::NotFound => "not_found",
        CodexOAuthAdminError::Conflict => "conflict",
        CodexOAuthAdminError::FlowExpired => "flow_expired",
        CodexOAuthAdminError::CallbackRejected => "callback_rejected",
        CodexOAuthAdminError::TokenRejected
        | CodexOAuthAdminError::Exchange(Exchange::Rejected) => "token_rejected",
        CodexOAuthAdminError::Exchange(Exchange::Unavailable) => "upstream_unavailable",
        CodexOAuthAdminError::Exchange(Exchange::Connection {
            timed_out: true, ..
        }) => "connect_timeout",
        CodexOAuthAdminError::Exchange(Exchange::Connection {
            timed_out: false, ..
        }) => "connect_failed",
        CodexOAuthAdminError::Exchange(Exchange::Ambiguous { .. }) => "ambiguous",
        CodexOAuthAdminError::StorageUnavailable => "storage_unavailable",
        CodexOAuthAdminError::Profile(_) => "profile_unavailable",
        CodexOAuthAdminError::Credential => "credential",
    }
}

pub(super) fn map_quota_error(error: CodexCredentialQuotaError) -> ProviderAdminError {
    use CodexCredentialQuotaError as Error;
    use ProviderAdminErrorKind as Kind;
    let (kind, public_message, upstream_message) = match error {
        Error::Profile(source) => {
            return provider_admin_error(Kind::Unavailable)
                .with_public_message("客户端身份不可用")
                .with_source(source);
        }
        Error::InvalidCredentialData => (Kind::Invalid, "凭据或额度数据无效", None),
        Error::NotFound => (Kind::NotFound, "账号不存在", None),
        Error::RevisionConflict => (Kind::Conflict, "凭据已更新，请刷新账号列表", None),
        Error::CredentialRefreshRequired => (Kind::Unavailable, "请先刷新令牌", None),
        Error::Repository(_) | Error::Store { .. } => (Kind::Unavailable, "额度查询暂不可用", None),
        Error::Upstream {
            status,
            code,
            detail,
        } => {
            // 公开提示只使用已知状态和错误码，原始上游材料留在内部诊断
            let (kind, public_message) = match (status, code.as_deref()) {
                (Some(401), Some("token_revoked")) => {
                    (Kind::BadGateway, "访问令牌已撤销，请刷新令牌或重新授权")
                }
                (Some(401), _) => (Kind::BadGateway, "账号认证失败，请检查授权状态"),
                (Some(403), _) => (Kind::BadGateway, "额度查询被拒绝，请检查账号权限"),
                (Some(429), _) => (Kind::BadGateway, "额度查询过于频繁，请稍后重试"),
                (Some(500..=599), _) => (Kind::BadGateway, "额度服务异常，请稍后重试"),
                _ => (Kind::Unavailable, "额度查询失败"),
            };
            let upstream_message =
                format!("OpenAI 额度查询失败：上游 HTTP {status:?}，code = {code:?}：{detail}");
            (kind, public_message, Some(upstream_message))
        }
    };
    let error = provider_admin_error(kind).with_public_message(public_message);
    match upstream_message {
        Some(message) => error.with_message(message),
        None => error,
    }
}

pub(super) fn map_profile_statistics_error(
    error: CodexProfileStatisticsError,
) -> ProviderAdminError {
    use CodexProfileStatisticsError as Error;

    match error {
        Error::InvalidCredentialData => provider_admin_error(ProviderAdminErrorKind::Invalid),
        Error::NotFound => provider_admin_error(ProviderAdminErrorKind::NotFound),
        Error::Store { .. } | Error::TransportUnavailable => {
            provider_admin_error(ProviderAdminErrorKind::Unavailable)
        }
        Error::CredentialRefreshRequired { upstream_body } => {
            let mut error = provider_admin_error(ProviderAdminErrorKind::CredentialRefreshRequired)
                .with_public_message("请先刷新令牌");
            if let Some(body) = upstream_body {
                error = error.with_message(format!(
                    "OpenAI profile-statistics upstream returned HTTP 401: {}",
                    bounded_upstream_body(&body)
                ));
            }
            error
        }
        Error::Upstream {
            status,
            body,
            retry_after_seconds,
        } => {
            let retry_after = retry_after_seconds
                .map(|seconds| format!("; retry-after={seconds}s"))
                .unwrap_or_default();
            provider_admin_error(ProviderAdminErrorKind::BadGateway)
                .with_public_message("资料查询失败")
                .with_message(format!(
                    "OpenAI profile-statistics upstream returned HTTP {status}{retry_after}: {}",
                    bounded_upstream_body(&body)
                ))
        }
    }
}

pub(super) fn map_profile_avatar_error(error: CodexProfileAvatarError) -> ProviderAdminError {
    match error {
        CodexProfileAvatarError::ProfileStatistics(error) => map_profile_statistics_error(error),
        CodexProfileAvatarError::Missing => provider_admin_error(ProviderAdminErrorKind::NotFound),
        CodexProfileAvatarError::InvalidSource | CodexProfileAvatarError::Upstream { .. } => {
            provider_admin_error(ProviderAdminErrorKind::BadGateway)
        }
        CodexProfileAvatarError::TransportUnavailable => {
            provider_admin_error(ProviderAdminErrorKind::Unavailable)
        }
    }
}

pub(super) fn map_reset_credits_error(error: CodexResetCreditsError) -> ProviderAdminError {
    use CodexResetCreditsError as Error;

    match error {
        Error::InvalidCredentialData => provider_admin_error(ProviderAdminErrorKind::Invalid),
        Error::NotFound => provider_admin_error(ProviderAdminErrorKind::NotFound),
        Error::Store { .. } | Error::TransportUnavailable => {
            provider_admin_error(ProviderAdminErrorKind::Unavailable)
        }
        Error::CredentialRefreshRequired { upstream_body } => {
            let mut error =
                provider_admin_error(ProviderAdminErrorKind::CredentialRefreshRequired);
            if let Some(body) = upstream_body {
                error = error.with_message(format!(
                    "OpenAI reset-credit upstream returned HTTP 401: {}",
                    bounded_upstream_body(&body)
                ));
            }
            error
        }
        Error::Upstream {
            status,
            body,
            retry_after_seconds,
        } => {
            let retry_after = retry_after_seconds
                .map(|seconds| format!("; retry-after={seconds}s"))
                .unwrap_or_default();
            provider_admin_error(ProviderAdminErrorKind::BadGateway).with_message(format!(
                "OpenAI reset-credit upstream returned HTTP {status}{retry_after}: {}",
                bounded_upstream_body(&body)
            ))
        }
        Error::ConsumeResultUnknown => provider_admin_error(ProviderAdminErrorKind::Ambiguous)
            .with_message(
                "OpenAI reset-credit consume result is unknown; refresh the credit list before retrying",
            ),
    }
}

fn bounded_upstream_body(body: &str) -> String {
    const MAX_ADMIN_UPSTREAM_BODY_BYTES: usize = 8 * 1024;
    if body.len() <= MAX_ADMIN_UPSTREAM_BODY_BYTES {
        return body.to_owned();
    }
    let mut end = MAX_ADMIN_UPSTREAM_BODY_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_owned()
}

pub(super) fn map_catalog_error(error: CodexCredentialCatalogError) -> ProviderAdminError {
    use crate::credential::CredentialRepositoryError;
    use CodexCredentialCatalogError as Error;
    use ProviderAdminErrorKind as Kind;
    let (kind, message) = match error {
        Error::Credential(source) => {
            let (kind, message) = match &source {
                CredentialRepositoryError::InvalidCredentialData(_) => {
                    (Kind::Invalid, "查询模型的凭据无效，请检查账号授权")
                }
                CredentialRepositoryError::RevisionConflict(_) => {
                    (Kind::Conflict, "账号已更新，请重新查询模型")
                }
                CredentialRepositoryError::Store(_) => (Kind::Unavailable, "账号存储暂不可用"),
            };
            return provider_admin_error(kind)
                .with_public_message(message)
                .with_source(source);
        }
        Error::InvalidEtag => (Kind::Invalid, "模型目录版本无效，请重新查询"),
        Error::NoEligibleCredential => (Kind::NotFound, "没有可查询模型的账号"),
        Error::ConcurrentUpdate => (Kind::Conflict, "模型目录正在更新，请稍后重试"),
        Error::Upstream { .. } => (Kind::Unavailable, "模型查询失败"),
        Error::Cache => (Kind::Unavailable, "模型目录缓存暂不可用"),
        Error::Profile(source) => {
            return provider_admin_error(Kind::Unavailable)
                .with_public_message("模型目录客户端身份不可用")
                .with_source(source);
        }
    };
    provider_admin_error(kind).with_public_message(message)
}

pub(super) fn map_client_profile_error(
    error: crate::profile::selection::ClientProfileError,
) -> ProviderAdminError {
    use crate::profile::selection::ClientProfileError;
    let message = match error {
        ClientProfileError::Invalid => "客户端身份字段或版本组合不合法",
        ClientProfileError::InvalidInstallationSeed => "安装身份种子必须是带连字符的 UUID",
        ClientProfileError::InvalidUserAgent => {
            "User-Agent 必须是 1 至 4096 字节的单行 ASCII 文本，且首尾不能含空白"
        }
        ClientProfileError::CompanionHeadersRequired => {
            "无法识别 User-Agent，请补充 originator 和有效的 Core version"
        }
        ClientProfileError::CompanionHeadersConflict => {
            "originator 或 Core version 与 User-Agent 不一致"
        }
        ClientProfileError::ReleaseUnavailable => {
            "此客户端、平台与架构尚无已核验发布版本，请选择固定版本或稍后重试"
        }
    };
    provider_admin_error(ProviderAdminErrorKind::Invalid).with_public_message(message)
}
