//! 402 恢复证据与 availability 转换。
//!
//! `QuotaExhausted -> Ready` 只接受同一 credential revision 的真实成功响应、
//! 新鲜快照显式 `allowed=true` 且无 exhaustion、或已到期旧 reset 配合未耗尽新快照。

use std::time::SystemTime;

use gateway_core::engine::credential::AccountAvailability;
use reqwest::StatusCode;
use serde_json::Value;

use super::snapshot::CodexQuotaFact;
use crate::transport::CodexClientError;

/// 402 恢复可接受的证据强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaRecoveryEvidence {
    /// 一次真实成功推理响应（最强证据）。
    SuccessfulResponse,
    /// 周期 worker 刷新出的新快照（需旧 reset 已到期佐证）。
    RefreshedSnapshot,
}

///
/// 只处理 `QuotaExhausted` 的恢复——429/触顶不再进入该状态，由 quota 数据
/// 驱动调度排除（限流不改变账号可用性）。`Invalid/Expired/Banned` 等终态由 selector 的
/// 成功响应恢复路径处理。
///
/// `previous_reset_at` 是写入新快照前读取的旧确认 reset；`RefreshedSnapshot`
/// 恢复要求它已到期（窗口真实滚动后新快照的 reset 指向下一个未来周期，
/// 不能用它判定旧耗尽窗口是否已结束）。
pub(crate) fn quota_success_state(
    current: AccountAvailability,
    fact: CodexQuotaFact,
    previous_reset_at: Option<SystemTime>,
    now: SystemTime,
    recovery_evidence: QuotaRecoveryEvidence,
) -> Option<AccountAvailability> {
    if current != AccountAvailability::QuotaExhausted || fact.exhausted() {
        return None;
    }
    // 新鲜快照显式 `allowed=true` 且无 exhaustion，直接恢复。
    if fact.explicit_allowed() {
        return Some(AccountAvailability::Ready);
    }
    quota_recovery_confirmed(recovery_evidence, previous_reset_at, now)
        .then_some(AccountAvailability::Ready)
}

fn quota_recovery_confirmed(
    evidence: QuotaRecoveryEvidence,
    previous_reset_at: Option<SystemTime>,
    now: SystemTime,
) -> bool {
    match evidence {
        // 一次真实成功响应是恢复的最强证据。
        QuotaRecoveryEvidence::SuccessfulResponse => true,
        // 周期刷新：旧确认 reset 已到期才恢复；used_percent 是可滞后、会取整的
        // 观测值，其回落不能覆盖仍未到期的旧 reset。
        QuotaRecoveryEvidence::RefreshedSnapshot => {
            previous_reset_at.is_some_and(|reset_at| reset_at <= now)
        }
    }
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

fn is_deactivated_workspace(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .is_some_and(|value| {
            value.pointer("/detail/code").and_then(Value::as_str) == Some("deactivated_workspace")
        })
}
