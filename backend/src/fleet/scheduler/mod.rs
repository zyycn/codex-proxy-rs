//! 智能账号调度模块。
//!
//! 账号选择的唯一归属地。把「选择」从账号池存储中剥离，集中于此：
//!
//! - [`candidates`] —— 候选过滤（可用性 / 层级 / 模型允许列表），所有策略共享的前置。
//! - [`strategy`] —— 全部选择策略，每个策略一个文件（[`strategy::smart`] /
//!   [`strategy::quota_reset`] / [`strategy::round_robin`] / [`strategy::sticky`]）。
//! - [`feedback`] —— 跨策略的运行时 EWMA 反馈（错误率 / TTFT），喂给 Smart 打分。
//!
//! [`AccountScheduler`] 是对外门面：持有轮转游标、打分权重与 EWMA 反馈存储，接收
//! **已过滤的候选切片**并返回选中账号。它不 lock 账号池、不刷状态、不写库——存储与
//! 状态维护仍归 `AccountPool`，二者经候选切片与 `slot_count` 闭包解耦。

pub mod candidates;
pub mod feedback;
pub mod strategy;

use std::{cmp::Ordering, sync::Mutex};

use chrono::{DateTime, Utc};

use crate::fleet::account::Account;
use crate::infra::time::parse_rfc3339_utc;

pub use candidates::{CandidateFilter, CandidateRequest};
pub use feedback::{FeedbackSample, FeedbackStats};
pub use strategy::smart::rank_candidates;
pub use strategy::{ScoreBreakdown, ScoreWeights};

/// 账号轮转策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStrategy {
    /// 加权线性打分调度。
    Smart,
    /// 优先消耗即将重置的额度窗口。
    QuotaResetPriority,
    /// 按候选账号顺序循环选择。
    RoundRobin,
    /// 优先选择最近使用过的账号。
    Sticky,
}

impl RotationStrategy {
    /// 返回策略的持久化与日志字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::QuotaResetPriority => "quota_reset_priority",
            Self::RoundRobin => "round_robin",
            Self::Sticky => "sticky",
        }
    }
}

/// 策略选择的运行时输入。
pub struct SelectionInput<'a> {
    /// 已过滤的候选账号集合。
    pub candidates: &'a [&'a Account],
    /// 按账号 ID 读取当前在途槽位数。
    pub slot_count: &'a dyn Fn(&str) -> usize,
    /// 账号 EWMA 反馈存储。
    pub feedback: &'a FeedbackStats,
    /// Smart 策略打分权重。
    pub weights: &'a ScoreWeights,
    /// 当前时刻。
    pub now: DateTime<Utc>,
}

/// 账号调度器：账号选择的对外门面。
///
/// 持有轮转游标（并列破并列用）、打分权重（Smart 用）与 EWMA 反馈存储。选择热路径
/// 只读反馈与权重，请求结束后经 [`AccountScheduler::report_feedback`] 回灌。游标用
/// [`Mutex`] 保护（选择需可变，但 `AccountScheduler` 以共享引用参与并发请求）。
#[derive(Debug)]
pub struct AccountScheduler {
    /// 轮转游标：在并列最优账号之间做公平轮转。
    cursor: Mutex<usize>,
    /// Smart 策略的打分权重。
    weights: ScoreWeights,
    /// 账号运行时 EWMA 反馈（错误率 / TTFT）。
    feedback: FeedbackStats,
}

impl Default for AccountScheduler {
    fn default() -> Self {
        Self::new(ScoreWeights::default())
    }
}

impl AccountScheduler {
    /// 使用指定打分权重创建调度器。
    pub fn new(weights: ScoreWeights) -> Self {
        Self {
            cursor: Mutex::new(0),
            weights,
            feedback: FeedbackStats::new(),
        }
    }

    /// 替换打分权重（策略切换或热更时调用）。
    pub fn set_weights(&mut self, weights: ScoreWeights) {
        self.weights = weights;
    }

    /// 重置轮转游标（策略切换时调用，保证轮转从头开始）。
    pub fn reset_cursor(&self) {
        *self.lock_cursor() = 0;
    }

    /// 从已过滤的候选集合中按指定策略选择一个账号。
    ///
    /// `candidates` 必须是 [`candidates::filter`] 的输出；`slot_count` 按账号 ID 读取
    /// 当前在途槽位数（Smart 打分与候选无关的运行时状态由调用方提供）。
    pub fn select(
        &self,
        strategy: RotationStrategy,
        candidates: &[&Account],
        slot_count: &dyn Fn(&str) -> usize,
        now: DateTime<Utc>,
    ) -> Option<Account> {
        let input = SelectionInput {
            candidates,
            slot_count,
            feedback: &self.feedback,
            weights: &self.weights,
            now,
        };
        let mut cursor = self.lock_cursor();
        select_with_strategy(strategy, &input, &mut cursor)
    }

    /// 回灌一次请求结果到运行时 EWMA 反馈（错误率 / TTFT），供 Smart 打分。
    pub fn report_feedback(&self, account_id: &str, success: bool, first_token_ms: Option<u64>) {
        self.feedback.report(account_id, success, first_token_ms);
    }

    /// 移除指定账号的运行时反馈（账号被删除时调用）。
    pub fn forget_feedback(&self, account_id: &str) {
        self.feedback.remove(account_id);
    }

    /// 清空全部运行时反馈（账号池清空时调用）。
    pub fn clear_feedback(&self) {
        self.feedback.clear();
    }

    fn lock_cursor(&self) -> std::sync::MutexGuard<'_, usize> {
        self.cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn select_with_strategy(
    strategy: RotationStrategy,
    input: &SelectionInput<'_>,
    cursor: &mut usize,
) -> Option<Account> {
    match strategy {
        RotationStrategy::Smart => strategy::smart::select(input, cursor),
        RotationStrategy::QuotaResetPriority => strategy::quota_reset::select(input, cursor),
        RotationStrategy::RoundRobin => strategy::round_robin::select(input, cursor),
        RotationStrategy::Sticky => strategy::sticky::select(input, cursor),
    }
}

pub(crate) fn select_by(
    candidates: &[&Account],
    cursor: &mut usize,
    compare: impl Fn(&Account, &Account) -> Ordering,
) -> Option<Account> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| compare(a, b));
    let best = *sorted.first()?;
    let tied_count = sorted
        .iter()
        .take_while(|account| compare(best, account) == Ordering::Equal)
        .count();
    let index = *cursor % tied_count;
    *cursor = cursor.wrapping_add(1);
    Some((*sorted[index]).clone())
}

pub(crate) fn account_window_token_count(account: &Account) -> u64 {
    account
        .window_input_tokens
        .saturating_add(account.window_output_tokens)
        .saturating_add(account.window_image_input_tokens)
        .saturating_add(account.window_image_output_tokens)
}

pub(crate) fn compare_window_reset(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_) | None, None) | (None, Some(_)) => Ordering::Equal,
    }
}

pub(crate) fn compare_last_used(a: Option<&str>, b: Option<&str>) -> Ordering {
    last_used_millis(a).cmp(&last_used_millis(b))
}

fn last_used_millis(value: Option<&str>) -> i64 {
    value
        .and_then(|value| parse_rfc3339_utc(value).ok())
        .map(|datetime| datetime.timestamp_millis())
        .unwrap_or(0)
}
