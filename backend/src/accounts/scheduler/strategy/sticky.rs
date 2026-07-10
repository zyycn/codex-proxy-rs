//! Sticky 策略：优先选择最近使用过的账号。
//!
//! 选中 `last_used_at` 最大（最近使用）的账号，倾向于持续复用同一账号（利于上游的
//! prompt 缓存命中）。该策略不使用轮转游标——最近使用时间本身即确定性顺序。

use std::cmp::Ordering;

use crate::accounts::account::Account;
use crate::accounts::scheduler::{compare_last_used, SelectionInput};

/// 选中最近使用过的账号（`last_used_at` 最大者）。
///
/// `_cursor` 未使用：Sticky 由 `last_used_at` 决定顺序，不参与轮转破并列。
pub fn select(input: &SelectionInput<'_>, _cursor: &mut usize) -> Option<Account> {
    let candidates = input.candidates;
    let mut selected = candidates.first()?;
    for candidate in &candidates[1..] {
        if compare_last_used(
            candidate.last_used_at.as_deref(),
            selected.last_used_at.as_deref(),
        ) == Ordering::Greater
        {
            selected = candidate;
        }
    }
    Some(selected.clone())
}
