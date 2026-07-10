//! RoundRobin 策略：按候选顺序循环选择。
//!
//! 最简单的策略：忽略账号负载与用量，纯粹按候选集合的顺序用轮转游标依次选中，
//! 保证请求在候选账号间均匀轮转。

use crate::accounts::account::Account;
use crate::accounts::scheduler::SelectionInput;

/// 按候选顺序做纯轮转选择。
pub fn select(input: &SelectionInput<'_>, cursor: &mut usize) -> Option<Account> {
    let candidates = input.candidates;
    if candidates.is_empty() {
        return None;
    }
    *cursor %= candidates.len();
    let index = *cursor;
    *cursor = cursor.wrapping_add(1);
    Some(candidates[index].clone())
}
