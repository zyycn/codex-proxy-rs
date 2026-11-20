//! 额度接口失败到规范化账号事实的唯一分类边界

use gateway_core::account::{AccountErrorReason, CredentialState, QuotaEvidence};
use reqwest::StatusCode;
use serde_json::Value;

use crate::transport::CodexClientError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaEndpointFailure {
    AccessTokenRevoked,
    Credential {
        state: CredentialState,
        reason: AccountErrorReason,
    },
    Exhausted(QuotaEvidence),
}

/// 仅依据明确的访问令牌撤销、工作区停用或额度拒绝证据更新对应事实
pub(crate) fn classify_quota_endpoint_failure(
    error: &CodexClientError,
) -> Option<QuotaEndpointFailure> {
    let CodexClientError::Upstream { status, body, .. } = error else {
        return None;
    };
    if *status == StatusCode::UNAUTHORIZED
        && upstream_error_code(error).as_deref() == Some("token_revoked")
    {
        return Some(QuotaEndpointFailure::AccessTokenRevoked);
    }
    if *status != StatusCode::PAYMENT_REQUIRED {
        return None;
    }
    if is_deactivated_workspace(body) {
        return Some(QuotaEndpointFailure::Credential {
            state: CredentialState::Banned,
            reason: AccountErrorReason::AccountBanned,
        });
    }
    Some(QuotaEndpointFailure::Exhausted(
        QuotaEvidence::PaymentRequired,
    ))
}

fn is_deactivated_workspace(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|value| {
            value.pointer("/detail/code").and_then(Value::as_str) == Some("deactivated_workspace")
        })
}

/// 从上游错误体提取稳定错误码：支持 error、detail 和顶层结构
///
/// 只接受有界的 ASCII 标识，避免把自由文本作为错误码写入诊断
pub(super) fn upstream_error_code(error: &CodexClientError) -> Option<String> {
    let CodexClientError::Upstream { body, .. } = error else {
        return None;
    };
    let value = serde_json::from_str::<Value>(body).ok()?;
    let code = value
        .pointer("/error/code")
        .or_else(|| value.pointer("/detail/code"))
        .or_else(|| value.pointer("/code"))
        .and_then(Value::as_str)?
        .trim();
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(code.to_owned())
}
