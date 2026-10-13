//! 验证额度窗口恢复的 reset 基线、低用量门槛与连续未触顶证据

use std::time::{Duration, SystemTime};

use gateway_core::account::{QuotaWindowRecovery, quota_window_recovery};

fn reset(seconds: u64) -> Option<SystemTime> {
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

#[test]
fn an_unknown_reset_only_establishes_a_baseline() {
    assert_eq!(
        quota_window_recovery(None, reset(200), Some(0.0), false, true),
        QuotaWindowRecovery::Pending,
    );
}

#[test]
fn a_rolled_window_requires_usage_below_ten_percent() {
    for (used, expected) in [
        (Some(9.99), QuotaWindowRecovery::Recovered),
        (Some(10.0), QuotaWindowRecovery::Pending),
        (Some(80.0), QuotaWindowRecovery::Pending),
        (None, QuotaWindowRecovery::Pending),
    ] {
        assert_eq!(
            quota_window_recovery(reset(100), reset(200), used, false, true),
            expected,
        );
    }
}

#[test]
fn an_unchanged_window_requires_two_consecutive_non_exhausted_observations() {
    assert_eq!(
        quota_window_recovery(reset(100), reset(100), Some(30.0), false, false),
        QuotaWindowRecovery::Candidate,
    );
    assert_eq!(
        quota_window_recovery(reset(100), reset(100), Some(35.0), false, true),
        QuotaWindowRecovery::Recovered,
    );
}

#[test]
fn renewed_exhaustion_interrupts_the_confirmation() {
    assert_eq!(
        quota_window_recovery(reset(100), reset(100), Some(100.0), true, true),
        QuotaWindowRecovery::Pending,
    );
}
