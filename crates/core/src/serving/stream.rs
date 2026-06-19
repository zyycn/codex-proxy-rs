//! 流生命周期策略。

/// 流结束原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFinishReason {
    /// 正常完成。
    Completed,
    /// 上游中断。
    Interrupted,
}
