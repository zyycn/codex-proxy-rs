//! 协议转换的结构错误

use thiserror::Error;

/// 可安全传递给协议边界的转换错误
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TranslationError {
    /// 请求结构无法解析
    #[error("{0}")]
    InvalidRequest(&'static str),
    /// 上游事件与已建立的流状态冲突
    #[error("{0}")]
    InvalidEvent(&'static str),
    /// 流在完整终态之前结束
    #[error("The upstream stream ended before its terminal event.")]
    IncompleteStream,
}
