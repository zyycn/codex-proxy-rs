//! 备份纯策略：Cron 推进与保留候选。
//!
//! 本模块不持有任何端口或对象，是 `BackupService` 与 `BackupTask` 共用的纯函数集。

use std::str::FromStr;

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use chrono_tz::Tz;
use cron::Schedule;

use crate::model::backup::{BackupError, BackupRecord, BackupStatus, code};

/// 向历史回溯的最大范围（分钟）。覆盖 Feb 29 + 特定星期等罕见组合的最长间隔。
const MAX_BACKDATE_MINUTES: i64 = 40 * 366 * 24 * 60;

/// 已解析并校验的备份计划。
#[derive(Debug, Clone)]
pub struct BackupSchedule {
    schedule: Schedule,
    timezone: Tz,
}

impl BackupSchedule {
    /// 解析并校验 5 段 Cron 与 IANA 时区。
    ///
    /// # Errors
    ///
    /// Cron 非法或时区不是 IANA 名称时返回 [`BackupError`]。
    pub fn parse(cron_expression: &str, schedule_timezone: &str) -> Result<Self, BackupError> {
        let expression = format!("0 {cron_expression}");
        let schedule = Schedule::from_str(&expression).map_err(|_| {
            BackupError::new(code::INVALID_CRON, "Cron 表达式必须是 5 段格式".to_owned())
        })?;
        let timezone = schedule_timezone.parse::<Tz>().map_err(|_| {
            BackupError::new(code::INVALID_TIMEZONE, "时区必须是 IANA 名称".to_owned())
        })?;
        Ok(Self { schedule, timezone })
    }

    /// 返回最接近 `at`（含 `at` 所在分钟）的最近一次触发时间。
    ///
    /// 若过去 `MAX_BACKDATE_MINUTES` 内没有任何触发（极罕见的稀疏计划或超长停机），
    /// 返回 `None`，调用方不应为历史点补任务。
    #[must_use]
    pub fn last_firing_at_or_before(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let high_min = at.timestamp().div_euclid(60);
        let low_min = high_min.saturating_sub(MAX_BACKDATE_MINUTES);

        // 检查窗口内是否存在触发；不存在则直接返回 None。
        let low_instant = instant(low_min, self.timezone)?;
        if self
            .schedule
            .after(&low_instant)
            .next()
            .is_none_or(|firing| firing.timestamp() > high_min * 60 + 59)
        {
            return None;
        }

        // 二分查找最小的 u（分钟）使 after(u) > high；prev = after(u - 1 分钟)。
        let mut lo = low_min;
        let mut hi = high_min;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self
                .firing_after(mid)
                .is_some_and(|firing| firing > high_min)
            {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        self.firing_after(lo - 1)
            .filter(|&firing| firing <= high_min)
            .and_then(|firing| Utc.timestamp_opt(firing * 60, 0).single())
    }

    /// 返回第一个严格晚于 `at` 的触发时间。
    #[must_use]
    pub fn next_after(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local = at.with_timezone(&self.timezone);
        self.schedule
            .after(&local)
            .next()
            .map(|firing| firing.with_timezone(&Utc))
    }

    /// `after(u 分钟)` 返回的第一个触发时间（分钟索引），用于二分搜索。
    fn firing_after(&self, minute_index: i64) -> Option<i64> {
        let instant = instant(minute_index, self.timezone)?;
        self.schedule
            .after(&instant)
            .next()
            .map(|firing| firing.timestamp().div_euclid(60))
    }
}

/// 构造指定分钟索引（UTC epoch 分钟）在目标时区下的 `DateTime`。
fn instant(minute_index: i64, timezone: Tz) -> Option<DateTime<Tz>> {
    timezone.timestamp_opt(minute_index * 60, 0).single()
}

/// 一条记录被纳入删除流程的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionReason {
    /// 超过 `retention_days`。
    Days,
    /// 超过 `retention_count`。
    Count,
}

/// 保留决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionDecision {
    pub record_id: String,
    pub reason: RetentionReason,
}

/// 从按完成时间倒序的 completed 计划备份中决定纳入删除流程的记录。
///
/// 自动保留规则：
/// - `retentionDays = 0` 时不按天数清理；`retentionCount = 0` 时不按份数清理。
/// - 自动计划备份满足任一启用阈值即可进入删除。
/// - 无论阈值如何，至少保留最近一个成功的计划备份（倒序下标 0）。
/// - 手动备份不进入本规则；手工/计划记录各自的 `expires_at` 由到期扫描处理。
#[must_use]
pub fn decide_retention(
    retention_days: u32,
    retention_count: u32,
    now: DateTime<Utc>,
    scheduled_completed_desc: &[BackupRecord],
) -> Vec<RetentionDecision> {
    let mut decisions = Vec::new();
    for (index, record) in scheduled_completed_desc.iter().enumerate() {
        if record.status != BackupStatus::Completed {
            continue;
        }
        // 最近一个成功的计划备份永远保留。
        if index == 0 {
            continue;
        }
        let expired_by_days = retention_days > 0
            && record.completed_at.is_some_and(|completed_at| {
                completed_at <= now - Duration::days(i64::from(retention_days))
            });
        let exceeded_count =
            retention_count > 0 && index >= usize::try_from(retention_count).unwrap_or(usize::MAX);
        if expired_by_days || exceeded_count {
            decisions.push(RetentionDecision {
                record_id: record.id.clone(),
                reason: if expired_by_days {
                    RetentionReason::Days
                } else {
                    RetentionReason::Count
                },
            });
        }
    }
    decisions
}
