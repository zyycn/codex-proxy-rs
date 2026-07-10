//! 账号调度策略集合。

pub mod quota_reset;
pub mod round_robin;
pub mod smart;
pub mod sticky;
mod types;

pub use smart::{ScoreBreakdown, ScoreWeights};
pub(crate) use types::{
    account_window_token_count, compare_last_used, compare_window_reset, select_by,
};
pub use types::{select, RotationStrategy, SelectionInput};
