//! QuotaResetPriority 策略：优先消耗即将重置的额度窗口。
//!
//! 排序规则（升序，越靠前越优先）：
//! 1. 未封顶配额优先于已封顶（`quota_limit_reached` false < true）；
//! 2. 窗口重置时间越早越优先；
//! 3. 历史请求数越少越优先；
//! 4. 最近使用时间越久越优先（LRU）。
//!
//! 并列最优的账号之间由 [`super::select_by`] 用轮转游标破并列。

use std::cmp::Ordering;

use crate::accounts::account::Account;
use crate::accounts::scheduler::{
    compare_last_used, compare_window_reset, select_by, SelectionInput,
};

/// QuotaResetPriority 策略选择。
pub fn select(input: &SelectionInput<'_>, cursor: &mut usize) -> Option<Account> {
    select_by(input.candidates, cursor, compare_quota_reset_priority)
}

/// 配额重置优先级比较：升序，最优（最应先用）排在最前。
fn compare_quota_reset_priority(a: &Account, b: &Account) -> Ordering {
    a.quota_limit_reached
        .cmp(&b.quota_limit_reached)
        .then_with(|| compare_window_reset(a.window_reset_at, b.window_reset_at))
        .then_with(|| a.request_count.cmp(&b.request_count))
        .then_with(|| compare_last_used(a.last_used_at.as_deref(), b.last_used_at.as_deref()))
}
