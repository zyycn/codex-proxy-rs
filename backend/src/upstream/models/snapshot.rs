//! 模型计划快照。

use serde::{Deserialize, Serialize};

use super::info::CodexModelInfo;

/// 按计划类型持久化的模型快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPlanSnapshot {
    /// 订阅计划类型。
    pub plan_type: String,
    /// 该计划可见的模型列表。
    pub models: Vec<CodexModelInfo>,
}
