//! 确认额度耗尽后的恢复证据与 availability 转换。
//!
//! `QuotaExhausted -> Ready` 只能由同一 credential revision 的权威 usage 快照证明：
//! 已确认的旧窗口 reset 已到期，且新快照的 reset 已推进到下一窗口。

use std::time::SystemTime;

use gateway_core::engine::credential::AccountAvailability;
use reqwest::StatusCode;
use serde_json::Value;

use super::snapshot::CodexQuotaFact;
use crate::transport::CodexClientError;

/// 只处理 `QuotaExhausted` 的恢复。`allowed=true`、成功响应和百分比回落都不能单独
/// 覆盖一次已确认的额度拒绝；它们在 reset 未推进时可能只是上游滞后的观测。
///
/// `previous_reset_at` 是写入新快照前读取的已确认耗尽窗口。恢复必须同时满足：
/// 旧窗口已经到期，且权威快照的 reset 指向比旧窗口更晚的新周期。
pub(crate) fn quota_success_state(
    current: AccountAvailability,
    fact: CodexQuotaFact,
    previous_reset_at: Option<SystemTime>,
    now: SystemTime,
) -> Option<AccountAvailability> {
    if current != AccountAvailability::QuotaExhausted || fact.exhausted() {
        return None;
    }
    let previous_reset_at = previous_reset_at?;
    let refreshed_reset_at = fact.resets_at().map(SystemTime::from)?;
    (previous_reset_at <= now && refreshed_reset_at > previous_reset_at)
        .then_some(AccountAvailability::Ready)
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
        // 429/503：临时限流，不写 availability（限流不改变账号可用性），由 quota 数据驱动。
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => None,
        // quota/usage 接口只能产生额度域状态；鉴权拒绝不具备判定 RT
        // 永久失效的证据，终态只能由 OAuth refresh/身份校验状态机写入。
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
