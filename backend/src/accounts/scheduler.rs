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

use std::sync::Mutex;

use chrono::{DateTime, Utc};

use crate::accounts::account::Account;

pub use candidates::{CandidateFilter, CandidateRequest};
pub use feedback::{FeedbackSample, FeedbackStats};
pub use strategy::smart::rank_candidates;
pub use strategy::{RotationStrategy, ScoreBreakdown, ScoreWeights};

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
        candidates: &[Account],
        slot_count: &dyn Fn(&str) -> usize,
        now: DateTime<Utc>,
    ) -> Option<Account> {
        let input = strategy::SelectionInput {
            candidates,
            slot_count,
            feedback: &self.feedback,
            weights: &self.weights,
            now,
        };
        let mut cursor = self.lock_cursor();
        strategy::select(strategy, &input, &mut cursor)
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
