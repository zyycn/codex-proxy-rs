use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;

/// Cloudflare Path Block Tracker
///
/// 追踪每个账户连续遇到的 Cloudflare path-block 404 错误。
/// 当一个账户连续遇到 3 次 path-block 时，自动将其标记为 Disabled。
///
/// Path-block 的特征：
/// - HTTP 404 状态码
/// - 空响应体或 Cloudflare 页面
/// - 通常发生在 /backend-api/responses 路径
const PATH_BLOCK_THRESHOLD: u32 = 3;
const PATH_BLOCK_STALE_AFTER: Duration = Duration::hours(1);

#[derive(Debug, Clone, Copy)]
struct PathBlockState {
    count: u32,
    last_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CfPathBlockTracker {
    /// 每个账户的连续 path-block 计数
    counts: Arc<RwLock<HashMap<String, PathBlockState>>>,
}

impl Default for CfPathBlockTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CfPathBlockTracker {
    pub fn new() -> Self {
        Self {
            counts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 记录一次 path-block 404
    ///
    /// 返回当前连续计数，调用者应该在达到阈值时采取行动
    pub async fn record_path_block(&self, account_id: &str) -> u32 {
        let mut counts = self.counts.write().await;
        let now = Utc::now();
        let count = counts
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map(|state| state.count + 1)
            .unwrap_or(1);
        counts.insert(
            account_id.to_string(),
            PathBlockState {
                count,
                last_at: now,
            },
        );
        count
    }

    /// 重置账户的 path-block 计数（成功请求后调用）
    pub async fn reset(&self, account_id: &str) {
        let mut counts = self.counts.write().await;
        counts.remove(account_id);
    }

    /// 获取账户当前的 path-block 计数
    pub async fn get_count(&self, account_id: &str) -> u32 {
        let counts = self.counts.read().await;
        counts
            .get(account_id)
            .filter(|state| {
                Utc::now().signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER
            })
            .map(|state| state.count)
            .unwrap_or(0)
    }

    /// 检查是否应该禁用账户
    pub async fn should_disable(&self, account_id: &str) -> bool {
        self.get_count(account_id).await >= PATH_BLOCK_THRESHOLD
    }

    /// 清除所有计数（用于测试或管理操作）
    pub async fn clear_all(&self) {
        let mut counts = self.counts.write().await;
        counts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_path_block_counter() {
        let tracker = CfPathBlockTracker::new();

        // 第一次
        let count = tracker.record_path_block("acct_1").await;
        assert_eq!(count, 1);
        assert!(!tracker.should_disable("acct_1").await);

        // 第二次
        let count = tracker.record_path_block("acct_1").await;
        assert_eq!(count, 2);
        assert!(!tracker.should_disable("acct_1").await);

        // 第三次 - 应该触发禁用
        let count = tracker.record_path_block("acct_1").await;
        assert_eq!(count, 3);
        assert!(tracker.should_disable("acct_1").await);
    }

    #[tokio::test]
    async fn test_reset_clears_count() {
        let tracker = CfPathBlockTracker::new();

        tracker.record_path_block("acct_1").await;
        tracker.record_path_block("acct_1").await;
        assert_eq!(tracker.get_count("acct_1").await, 2);

        // 重置后计数应该清零
        tracker.reset("acct_1").await;
        assert_eq!(tracker.get_count("acct_1").await, 0);
    }

    #[tokio::test]
    async fn test_multiple_accounts() {
        let tracker = CfPathBlockTracker::new();

        tracker.record_path_block("acct_1").await;
        tracker.record_path_block("acct_2").await;
        tracker.record_path_block("acct_2").await;

        assert_eq!(tracker.get_count("acct_1").await, 1);
        assert_eq!(tracker.get_count("acct_2").await, 2);
    }
}
