//! 账号调度策略层。
//!
//! 所有账号选择策略集中于此，每个策略一个文件，互不依赖：
//!
//! - [`smart`] —— 加权线性打分 + EWMA 反馈（“智能调度”）。
//! - [`quota_reset`] —— 优先消耗即将重置的额度窗口。
//! - [`round_robin`] —— 按候选顺序循环。
//! - [`sticky`] —— 优先最近使用过的账号。
//!
//! 本模块只承担三件事：定义 [`RotationStrategy`] 枚举、把选择请求分派到对应策略、
//! 提供各策略共享的 tie-break 比较原语（[`select_by`] / [`compare_window_reset`] /
//! [`compare_last_used`]）。策略本身不触碰账号池存储：在途槽位数经 `slot_count`
//! 闭包按需读取，EWMA 反馈与打分权重经 [`SelectionInput`] 传入。

use std::cmp::Ordering;

use chrono::{DateTime, Utc};

use crate::accounts::account::Account;
use crate::accounts::scheduler::feedback::FeedbackStats;
use crate::infra::time::parse_rfc3339_utc;

use super::smart::ScoreWeights;

/// 账号轮转策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStrategy {
    /// 加权线性打分调度：多因子（负载/窗口/错误率/TTFT/重置）归一化后加权求和，
    /// 配合 EWMA 反馈。
    Smart,
    /// 优先消耗即将重置的额度窗口。
    QuotaResetPriority,
    /// 按候选账号顺序循环选择。
    RoundRobin,
    /// 优先选择最近使用过的账号。
    Sticky,
}

impl RotationStrategy {
    /// 返回策略的持久化/日志字符串。
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
///
/// `slot_count` 按账号 ID 读取当前在途槽位数；`feedback` / `weights` 仅 [`smart`]
/// 使用，其余策略忽略；`now` 用于窗口重置相关的打分。
pub struct SelectionInput<'a> {
    /// 已过滤的候选账号集合。
    pub candidates: &'a [Account],
    /// 按账号 ID 读取当前在途槽位数。
    pub slot_count: &'a dyn Fn(&str) -> usize,
    /// 账号 EWMA 反馈存储（Smart 打分用）。
    pub feedback: &'a FeedbackStats,
    /// 打分权重（Smart 打分用）。
    pub weights: &'a ScoreWeights,
    /// 当前时刻（窗口重置因子用）。
    pub now: DateTime<Utc>,
}

/// 按指定策略从候选集合中选择一个账号。
///
/// `cursor` 是账号池持有的轮转游标，用于在并列最优账号之间做公平轮转；不同策略对它
/// 的读写方式不同（详见各策略模块），本函数只负责透传可变引用。
pub fn select(
    strategy: RotationStrategy,
    input: &SelectionInput<'_>,
    cursor: &mut usize,
) -> Option<Account> {
    match strategy {
        RotationStrategy::Smart => super::smart::select(input, cursor),
        RotationStrategy::QuotaResetPriority => super::quota_reset::select(input, cursor),
        RotationStrategy::RoundRobin => super::round_robin::select(input, cursor),
        RotationStrategy::Sticky => super::sticky::select(input, cursor),
    }
}

// ====================================================================
// 共享 tie-break 原语
// ====================================================================

/// 按比较函数升序排序后，在并列最优（`Ordering::Equal`）的账号之间用轮转游标破并列。
///
/// 这是 [`smart`] 与 [`quota_reset`] 共享的选择骨架：先按各自的比较规则排序，再在得分
/// 相同的账号之间轮转，保证并列账号被公平地依次选中。
pub(crate) fn select_by(
    candidates: &[Account],
    cursor: &mut usize,
    compare: impl Fn(&Account, &Account) -> Ordering,
) -> Option<Account> {
    let mut sorted = candidates.iter().collect::<Vec<_>>();
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

/// 计算账号当前窗口的总 token 数（输入 + 输出 + 图片输入 + 图片输出）。
pub(crate) fn account_window_token_count(account: &Account) -> u64 {
    account
        .window_input_tokens
        .saturating_add(account.window_output_tokens)
        .saturating_add(account.window_image_input_tokens)
        .saturating_add(account.window_image_output_tokens)
}

/// 比较两个窗口重置时间：越早重置越优先；缺失重置时间视为并列（不惩罚）。
pub(crate) fn compare_window_reset(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_) | None, None) | (None, Some(_)) => Ordering::Equal,
    }
}

/// 比较两个账号的最近使用时间：越久未用越优先（升序，最小者排前）。
pub(crate) fn compare_last_used(a: Option<&str>, b: Option<&str>) -> Ordering {
    last_used_millis(a).cmp(&last_used_millis(b))
}

fn last_used_millis(value: Option<&str>) -> i64 {
    value
        .and_then(|value| parse_rfc3339_utc(value).ok())
        .map(|datetime| datetime.timestamp_millis())
        .unwrap_or(0)
}
