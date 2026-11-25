//! Provider 额度文档、历史采样与预热状态的持久化入口

mod document;
mod forecast;
mod history;
mod warmup;

pub(super) use document::{versions, write};
pub(super) use forecast::load_history;
pub(super) use history::documents;
