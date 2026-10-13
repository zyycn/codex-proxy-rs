//! 已耗尽额度窗口的恢复证据，供账号状态与只读预测共用

use std::time::SystemTime;

const RESET_RECOVERY_MAX_USED_PERCENT: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWindowRecovery {
    Pending,
    Candidate,
    Recovered,
}

/// 调用方先确认窗口曾耗尽，再按连续观测积累恢复证据
/// 缺少旧 reset 时只建立基线，不把首次低用量观测当作恢复
#[must_use]
pub fn quota_window_recovery(
    previous_reset: Option<SystemTime>,
    current_reset: Option<SystemTime>,
    used_percent: Option<f64>,
    limit_reached: bool,
    previous_candidate: bool,
) -> QuotaWindowRecovery {
    let Some(previous_reset) = previous_reset else {
        return QuotaWindowRecovery::Pending;
    };
    let rolled_forward = current_reset.is_some_and(|current| current > previous_reset);
    if rolled_forward {
        // 高用量下的 reset 前进不足以证明恢复，保留原耗尽基线
        if used_percent.is_some_and(|used| used < RESET_RECOVERY_MAX_USED_PERCENT) {
            QuotaWindowRecovery::Recovered
        } else {
            QuotaWindowRecovery::Pending
        }
    } else if !limit_reached {
        // 连续两次未触顶，排除耗尽后立刻收到的一次旧快照
        if previous_candidate {
            QuotaWindowRecovery::Recovered
        } else {
            QuotaWindowRecovery::Candidate
        }
    } else {
        QuotaWindowRecovery::Pending
    }
}
