//! 额度接口失败到规范化账号事实的唯一分类边界。

use gateway_core::engine::credential::{AccountErrorReason, CredentialState, QuotaEvidence};
use reqwest::StatusCode;
use serde_json::Value;

use crate::transport::CodexClientError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaEndpointFailure {
    Credential {
        state: CredentialState,
        reason: AccountErrorReason,
    },
    Exhausted(QuotaEvidence),
}

/// 额度 endpoint 只有明确 402 能改变账号事实。401/403、429、5xx 与传输失败
/// 均不足以判断凭据或账号状态，留给 OAuth 与真实推理请求确认。
pub(crate) fn classify_quota_endpoint_failure(
    error: &CodexClientError,
) -> Option<QuotaEndpointFailure> {
    let CodexClientError::Upstream { status, body, .. } = error else {
        return None;
    };
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
