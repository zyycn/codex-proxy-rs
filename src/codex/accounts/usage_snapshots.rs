use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 使用历史快照 - 时间序列统计
///
/// 记录账户使用情况的快照，支持多种时间粒度的聚合：
/// - raw: 原始快照（每次记录）
/// - five_min: 5分钟聚合
/// - hourly: 小时聚合
/// - daily: 天聚合

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub timestamp: DateTime<Utc>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub request_count: i64,
    pub active_accounts: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Raw,
    FiveMin,
    Hourly,
    Daily,
}

impl Granularity {
    pub fn bucket_duration(&self) -> Duration {
        match self {
            Granularity::Raw => Duration::zero(),
            Granularity::FiveMin => Duration::minutes(5),
            Granularity::Hourly => Duration::hours(1),
            Granularity::Daily => Duration::days(1),
        }
    }

    /// 将时间戳对齐到桶边界
    pub fn align_timestamp(&self, ts: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Granularity::Raw => ts,
            Granularity::FiveMin => {
                let minutes = (ts.minute() / 5) * 5;
                align_utc_time(ts, ts.hour(), minutes, 0)
            }
            Granularity::Hourly => align_utc_time(ts, ts.hour(), 0, 0),
            Granularity::Daily => align_utc_time(ts, 0, 0, 0),
        }
    }
}

fn align_utc_time(ts: DateTime<Utc>, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    ts.date_naive()
        .and_hms_opt(hour, minute, second)
        .map(|value| value.and_utc())
        .unwrap_or(ts)
}

#[derive(Debug, Clone)]
pub struct UsageSnapshotStore {
    snapshots: VecDeque<UsageSnapshot>,
    max_snapshots: usize,
    retention_days: u64,
}

impl UsageSnapshotStore {
    pub fn new(max_snapshots: usize, retention_days: u64) -> Self {
        Self {
            snapshots: VecDeque::with_capacity(max_snapshots),
            max_snapshots,
            retention_days,
        }
    }

    /// 添加新的快照
    pub fn push(&mut self, snapshot: UsageSnapshot) {
        // 清理过期快照
        self.cleanup_expired();

        // 添加新快照
        self.snapshots.push_back(snapshot);

        // 限制最大数量
        while self.snapshots.len() > self.max_snapshots {
            self.snapshots.pop_front();
        }
    }

    /// 清理过期的快照
    fn cleanup_expired(&mut self) {
        let cutoff = Utc::now() - Duration::days(self.retention_days as i64);
        while let Some(snapshot) = self.snapshots.front() {
            if snapshot.timestamp < cutoff {
                self.snapshots.pop_front();
            } else {
                break;
            }
        }
    }

    /// 获取指定时间范围和粒度的快照
    pub fn get_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        granularity: Granularity,
    ) -> Vec<UsageSnapshot> {
        if granularity == Granularity::Raw {
            // 原始快照 - 直接过滤
            return self
                .snapshots
                .iter()
                .filter(|s| s.timestamp >= start && s.timestamp <= end)
                .cloned()
                .collect();
        }

        // 聚合快照
        self.aggregate(start, end, granularity)
    }

    /// 聚合快照到指定粒度
    fn aggregate(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        granularity: Granularity,
    ) -> Vec<UsageSnapshot> {
        use std::collections::HashMap;

        let mut buckets: HashMap<DateTime<Utc>, UsageSnapshot> = HashMap::new();

        for snapshot in self.snapshots.iter() {
            if snapshot.timestamp < start || snapshot.timestamp > end {
                continue;
            }

            let bucket_key = granularity.align_timestamp(snapshot.timestamp);
            let entry = buckets.entry(bucket_key).or_insert_with(|| UsageSnapshot {
                timestamp: bucket_key,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                request_count: 0,
                active_accounts: 0,
            });

            entry.input_tokens += snapshot.input_tokens;
            entry.output_tokens += snapshot.output_tokens;
            entry.cached_tokens += snapshot.cached_tokens;
            entry.request_count += snapshot.request_count;
            entry.active_accounts = entry.active_accounts.max(snapshot.active_accounts);
        }

        let mut result: Vec<_> = buckets.into_values().collect();
        result.sort_by_key(|s| s.timestamp);
        result
    }

    /// 获取所有快照
    pub fn all(&self) -> Vec<UsageSnapshot> {
        self.snapshots.iter().cloned().collect()
    }

    /// 获取快照数量
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// 检查是否为空
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// 清空所有快照
    pub fn clear(&mut self) {
        self.snapshots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_granularity_alignment() {
        let ts = Utc::now()
            .date_naive()
            .and_hms_opt(14, 37, 42)
            .unwrap()
            .and_utc();

        // 5分钟对齐：14:37:42 -> 14:35:00
        let aligned = Granularity::FiveMin.align_timestamp(ts);
        assert_eq!(aligned.minute(), 35);
        assert_eq!(aligned.second(), 0);

        // 小时对齐：14:37:42 -> 14:00:00
        let aligned = Granularity::Hourly.align_timestamp(ts);
        assert_eq!(aligned.hour(), 14);
        assert_eq!(aligned.minute(), 0);

        // 天对齐：14:37:42 -> 00:00:00
        let aligned = Granularity::Daily.align_timestamp(ts);
        assert_eq!(aligned.hour(), 0);
        assert_eq!(aligned.minute(), 0);
    }

    #[test]
    fn test_snapshot_store() {
        let mut store = UsageSnapshotStore::new(100, 30);

        let now = Utc::now();
        store.push(UsageSnapshot {
            timestamp: now,
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 10,
            request_count: 1,
            active_accounts: 5,
        });

        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn test_aggregation() {
        let mut store = UsageSnapshotStore::new(1000, 30);
        let base = Utc::now()
            .date_naive()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc();

        // 添加多个快照
        for i in 0..10 {
            store.push(UsageSnapshot {
                timestamp: base + Duration::minutes(i * 3),
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: 10,
                request_count: 1,
                active_accounts: 5,
            });
        }

        // 获取5分钟聚合
        let aggregated = store.get_range(base, base + Duration::minutes(30), Granularity::FiveMin);

        // 应该有多个5分钟桶
        assert!(!aggregated.is_empty());
    }

    #[test]
    fn test_retention() {
        let mut store = UsageSnapshotStore::new(100, 1); // 1天保留期

        let now = Utc::now();
        let old = now - Duration::days(2); // 2天前

        // 添加过期快照
        store.push(UsageSnapshot {
            timestamp: old,
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 10,
            request_count: 1,
            active_accounts: 5,
        });

        // 添加新快照（会触发清理）
        store.push(UsageSnapshot {
            timestamp: now,
            input_tokens: 200,
            output_tokens: 100,
            cached_tokens: 20,
            request_count: 2,
            active_accounts: 10,
        });

        // 应该只剩新快照
        assert_eq!(store.len(), 1);
        assert_eq!(store.all()[0].input_tokens, 200);
    }
}
