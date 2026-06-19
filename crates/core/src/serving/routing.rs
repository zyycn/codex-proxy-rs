//! 账号选择路由策略。

/// 路由决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    /// 选中的账号 ID。
    pub account_id: String,
}
