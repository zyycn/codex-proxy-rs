//! 管理员会话领域逻辑。

use chrono::{DateTime, Duration, Utc};

/// 管理员会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSession {
    /// 会话 ID。
    pub id: String,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
}

impl AdminSession {
    /// 判断会话在给定时间是否已经过期。
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// 根据 TTL 计算会话过期时间。
pub fn session_expiry(now: DateTime<Utc>, ttl_minutes: i64) -> DateTime<Utc> {
    now + Duration::minutes(ttl_minutes.max(0))
}
