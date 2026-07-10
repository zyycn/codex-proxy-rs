//! 账号累计用量与时间桶查询。

use chrono::{DateTime, Utc};

pub mod query;
pub mod store;

/// 根据额度窗口边界漂移判断是否重置本地窗口计数。
pub fn should_reset_usage_window(
    existing_reset_at: Option<DateTime<Utc>>,
    existing_limit_window_seconds: Option<u64>,
    new_reset_at: DateTime<Utc>,
    new_limit_window_seconds: Option<u64>,
) -> bool {
    let Some(existing_reset_at) = existing_reset_at else {
        return false;
    };
    if existing_reset_at == new_reset_at {
        return false;
    }

    let drift = existing_reset_at
        .signed_duration_since(new_reset_at)
        .num_seconds()
        .unsigned_abs();
    let window_seconds = new_limit_window_seconds
        .or(existing_limit_window_seconds)
        .unwrap_or(0);
    let threshold = if window_seconds > 0 {
        window_seconds / 2
    } else {
        3_600
    };
    drift >= threshold
}
