//! Attempt 阶段的账号作用域上游调用与事实规范化。

pub(in crate::dispatch) mod account;
pub(crate) mod canonical;
pub(in crate::dispatch) mod observation;
pub(in crate::dispatch) mod prefetch;
