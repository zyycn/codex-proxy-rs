//! 对外服务编排策略。

/// 亲和性策略。
pub mod affinity;
/// 回退策略。
pub mod fallback;
/// Responses 隐式续接策略。
pub mod implicit_resume;
/// 配额策略。
pub mod quota;
/// Responses reasoning replay 缓存策略。
pub mod reasoning_replay;
/// 错误恢复策略。
pub mod recovery;
/// Responses 编排。
pub mod responses;
