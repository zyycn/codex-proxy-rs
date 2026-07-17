//! 上游失败到账号状态 effect 的唯一领域分类。

use chrono::{Duration, Utc};

use crate::fleet::{
    account::AccountStatus,
    account_gateway::{AccountFailureObservation, AccountUpstreamGateway},
    pool::AccountPoolService,
};

const DEFAULT_RATE_LIMIT_RETRY_SECONDS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFailureKind {
    ModelUnsupported,
    Expired,
    Disabled,
    Banned,
    QuotaExhausted,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountStateEffect {
    SetStatus(AccountStatus),
    MarkQuotaLimitedUntil(chrono::DateTime<Utc>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedAccountFailure {
    pub kind: AccountFailureKind,
    pub effect: Option<AccountStateEffect>,
}

impl ClassifiedAccountFailure {
    pub fn account_status(&self) -> Option<AccountStatus> {
        match self.effect.as_ref()? {
            AccountStateEffect::SetStatus(status) => Some(*status),
            AccountStateEffect::MarkQuotaLimitedUntil(_) => None,
        }
    }
}

pub fn classify_account_failure(
    facts: &AccountFailureObservation,
) -> Option<ClassifiedAccountFailure> {
    classify_failure(FailureFields {
        status_code: facts.status_code,
        code: facts.code.as_deref(),
        error_type: facts.error_type.as_deref(),
        identity_authorization_error: facts.identity_authorization_error.as_deref(),
        identity_error_code: facts.identity_error_code.as_deref(),
        message: &facts.message,
        body: &facts.body,
        retry_after_seconds: facts.retry_after_seconds,
    })
}

pub async fn apply_account_state_effect_immediately(
    account_pool: &AccountPoolService,
    upstream: &dyn AccountUpstreamGateway,
    account_id: &str,
    effect: &AccountStateEffect,
) {
    match effect {
        AccountStateEffect::SetStatus(status) => {
            account_pool
                .set_status_immediately(account_id, *status)
                .await;
        }
        AccountStateEffect::MarkQuotaLimitedUntil(until) => {
            account_pool
                .mark_quota_limited_until_immediately(account_id, *until)
                .await;
        }
    }
    upstream.evict_account_connections(account_id).await;
}

pub async fn apply_account_state_effect(
    account_pool: &AccountPoolService,
    upstream: &dyn AccountUpstreamGateway,
    account_id: &str,
    effect: &AccountStateEffect,
) -> bool {
    let updated = match effect {
        AccountStateEffect::SetStatus(status) => account_pool.set_status(account_id, *status).await,
        AccountStateEffect::MarkQuotaLimitedUntil(until) => {
            account_pool
                .mark_quota_limited_until(account_id, *until)
                .await
        }
    };
    upstream.evict_account_connections(account_id).await;
    updated
}

struct FailureFields<'a> {
    status_code: Option<u16>,
    code: Option<&'a str>,
    error_type: Option<&'a str>,
    identity_authorization_error: Option<&'a str>,
    identity_error_code: Option<&'a str>,
    message: &'a str,
    body: &'a str,
    retry_after_seconds: Option<u64>,
}

fn classify_failure(fields: FailureFields<'_>) -> Option<ClassifiedAccountFailure> {
    let code = fields.code.unwrap_or_default();
    let error_type = fields.error_type.unwrap_or_default();
    let identity_error_code = fields.identity_error_code.unwrap_or_default();
    let identity_authorization_error = fields.identity_authorization_error.unwrap_or_default();

    if [code, fields.message, fields.body]
        .into_iter()
        .any(is_model_unsupported)
    {
        return Some(classified(AccountFailureKind::ModelUnsupported, None));
    }
    if let Some(status) =
        explicit_account_status(identity_error_code, "", identity_authorization_error)
            .or_else(|| explicit_account_status(code, error_type, fields.message))
            .or_else(|| explicit_account_status(code, error_type, fields.body))
    {
        return Some(status_failure(status));
    }
    if fields.status_code == Some(401) {
        return Some(status_failure(AccountStatus::Expired));
    }

    let quota_kind = [code, error_type]
        .into_iter()
        .find_map(classify_quota_signal)
        .or_else(|| classify_quota_message(fields.message))
        .or_else(|| classify_quota_message(fields.body))
        .or(match fields.status_code {
            Some(429) => Some(AccountFailureKind::RateLimited),
            Some(402) => Some(AccountFailureKind::QuotaExhausted),
            _ => None,
        })?;
    Some(match quota_kind {
        AccountFailureKind::RateLimited => {
            let seconds = fields
                .retry_after_seconds
                .unwrap_or(DEFAULT_RATE_LIMIT_RETRY_SECONDS)
                .min(i64::MAX as u64) as i64;
            classified(
                AccountFailureKind::RateLimited,
                Some(AccountStateEffect::MarkQuotaLimitedUntil(
                    Utc::now() + Duration::seconds(seconds),
                )),
            )
        }
        AccountFailureKind::QuotaExhausted => classified(
            AccountFailureKind::QuotaExhausted,
            Some(AccountStateEffect::SetStatus(AccountStatus::QuotaExhausted)),
        ),
        AccountFailureKind::ModelUnsupported
        | AccountFailureKind::Expired
        | AccountFailureKind::Disabled
        | AccountFailureKind::Banned => unreachable!("quota classifier returned account status"),
    })
}

fn status_failure(status: AccountStatus) -> ClassifiedAccountFailure {
    let kind = match status {
        AccountStatus::Expired => AccountFailureKind::Expired,
        AccountStatus::Disabled => AccountFailureKind::Disabled,
        AccountStatus::Banned => AccountFailureKind::Banned,
        AccountStatus::Active | AccountStatus::QuotaExhausted => {
            unreachable!("explicit account failure must make the account unavailable")
        }
    };
    classified(kind, Some(AccountStateEffect::SetStatus(status)))
}

fn classified(
    kind: AccountFailureKind,
    effect: Option<AccountStateEffect>,
) -> ClassifiedAccountFailure {
    ClassifiedAccountFailure { kind, effect }
}

fn explicit_account_status(code: &str, error_type: &str, message: &str) -> Option<AccountStatus> {
    let code = code.trim().to_ascii_lowercase();
    let error_type = error_type.trim().to_ascii_lowercase();
    let message = message.to_ascii_lowercase();
    if matches!(
        code.as_str(),
        "identity_verification_required" | "verification_required"
    ) || message.contains("identity verification is required")
    {
        return Some(AccountStatus::Disabled);
    }
    if matches!(
        code.as_str(),
        "account_banned"
            | "account_deactivated"
            | "account_disabled"
            | "account_suspended"
            | "deactivated_workspace"
            | "organization_disabled"
            | "workspace_deactivated"
    ) || message.contains("account is banned")
        || message.contains("account has been banned")
        || message.contains("account deactivated")
        || message.contains("account has been deactivated")
        || message.contains("account disabled")
        || message.contains("account has been disabled")
        || message.contains("account suspended")
        || message.contains("organization has been disabled")
        || message.contains("workspace has been deactivated")
        || message.contains("deactivated_workspace")
    {
        return Some(AccountStatus::Banned);
    }
    is_auth_failure(&code, &error_type, &message).then_some(AccountStatus::Expired)
}

fn is_auth_failure(code: &str, error_type: &str, message: &str) -> bool {
    matches!(
        code,
        "token_invalid"
            | "token_invalidated"
            | "token_expired"
            | "token_revoked"
            | "refresh_token_invalidated"
            | "unauthorized"
            | "invalid_api_key"
            | "authentication_error"
    ) || error_type == "authentication_error"
        || message.contains("token revoked")
        || message.contains("token invalidated")
        || message.contains("token invalid")
        || message.contains("token expired")
        || message.contains("unauthorized")
        || message.contains("invalid api key")
}

fn classify_quota_signal(signal: &str) -> Option<AccountFailureKind> {
    let signal = signal.trim().to_ascii_lowercase();
    match signal.as_str() {
        "usage_limit_reached"
        | "rate_limit_exceeded"
        | "rate_limit_reached"
        | "rate_limit_error"
        | "workspace_owner_usage_limit_reached"
        | "workspace_member_usage_limit_reached" => Some(AccountFailureKind::RateLimited),
        "quota_exhausted"
        | "quota_exceeded"
        | "payment_required"
        | "insufficient_quota"
        | "workspace_owner_credits_depleted"
        | "workspace_member_credits_depleted" => Some(AccountFailureKind::QuotaExhausted),
        signal if signal.starts_with("billing_limit") => Some(AccountFailureKind::QuotaExhausted),
        _ => None,
    }
}

fn classify_quota_message(message: &str) -> Option<AccountFailureKind> {
    let message = message.to_ascii_lowercase();
    if message.contains("rate limit") || message.contains("usage limit") {
        return Some(AccountFailureKind::RateLimited);
    }
    if message.contains("quota")
        || message.contains("payment required")
        || message.contains("billing limit")
    {
        return Some(AccountFailureKind::QuotaExhausted);
    }
    None
}

fn is_model_unsupported(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("model_not_supported")
        || value.contains("model_not_available")
        || (value.contains("model")
            && (value.contains("not supported")
                || value.contains("not available")
                || value.contains("not_supported")
                || value.contains("not_available")))
}
