//! 账号调度读取 typed quota 的稳定策略入口。

use chrono::{DateTime, Utc};

use super::QuotaSnapshot;

pub fn quota_snapshot_limit_reached(quota: &QuotaSnapshot) -> bool {
    quota.is_limit_reached()
}

pub fn quota_snapshot_reset_at(quota: &QuotaSnapshot) -> Option<DateTime<Utc>> {
    quota.reset_at()
}

pub fn quota_snapshot_limit_window_seconds(quota: &QuotaSnapshot) -> Option<u64> {
    quota.limit_window_seconds()
}
