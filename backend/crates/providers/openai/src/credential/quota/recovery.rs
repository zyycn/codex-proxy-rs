//! 确认额度耗尽后的恢复证据与 availability 转换。
//!
//! `QuotaExhausted -> Ready` 只能由同一 credential revision 的权威 usage 快照证明：
//! 新快照未耗尽，且 reset 已推进到下一窗口。

use std::time::SystemTime;

use gateway_core::engine::credential::AccountAvailability;
use reqwest::StatusCode;
use serde_json::Value;

use super::snapshot::CodexQuotaFact;
use crate::transport::CodexClientError;

/// 只处理 `QuotaExhausted` 的恢复。`allowed=true`、成功响应和百分比回落都不能单独覆盖
/// 一次已确认的额度拒绝；它们在 reset 未推进时可能只是上游滞后的观测。
///
/// `previous_reset_at` 是写入新快照前读取的已确认耗尽窗口。恢复只要求权威快照的 reset
/// 指向比旧窗口更晚的新周期，不等待旧 reset 的墙钟时间到达。
pub(crate) fn quota_success_state(
    current: AccountAvailability,
    fact: CodexQuotaFact,
    previous_reset_at: Option<SystemTime>,
) -> Option<AccountAvailability> {
    if current != AccountAvailability::QuotaExhausted || fact.exhausted() {
        return None;
    }
    let previous_reset_at = previous_reset_at?;
    let refreshed_reset_at = fact.resets_at().map(SystemTime::from)?;
    (refreshed_reset_at > previous_reset_at).then_some(AccountAvailability::Ready)
}

pub(crate) fn quota_state_transition(error: &CodexClientError) -> Option<AccountAvailability> {
    let CodexClientError::Upstream { status, body, .. } = error else {
        return None;
    };
    if *status == StatusCode::PAYMENT_REQUIRED && is_deactivated_workspace(body) {
        return Some(AccountAvailability::Banned);
    }
    match *status {
        // 402：真额度耗尽 → QuotaExhausted（worker 按 reset_at 自动恢复）。
        StatusCode::PAYMENT_REQUIRED => Some(AccountAvailability::QuotaExhausted),
        // usage 401/403 不能单独判定账号不可用：保持现有状态，交给 OAuth 或真实请求确认。
        // 429/503 同样不写 availability，仅记失败待重试。
        StatusCode::UNAUTHORIZED
        | StatusCode::FORBIDDEN
        | StatusCode::TOO_MANY_REQUESTS
        | StatusCode::SERVICE_UNAVAILABLE => None,
        _ => None,
    }
}

/// 配额接口将账号变为非 ready 状态时可安全持久化的上游原因码。
pub(crate) fn quota_state_reason(error: &CodexClientError) -> Option<&'static str> {
    match quota_state_transition(error)? {
        AccountAvailability::Banned => Some("deactivated_workspace"),
        AccountAvailability::QuotaExhausted => Some("payment_required"),
        AccountAvailability::Unknown
        | AccountAvailability::Ready
        | AccountAvailability::Expired
        | AccountAvailability::Invalid => None,
    }
}

fn is_deactivated_workspace(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|value| {
            value.pointer("/detail/code").and_then(Value::as_str) == Some("deactivated_workspace")
        })
}
