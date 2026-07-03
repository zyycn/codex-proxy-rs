//! 标准化模型展示信息。

use serde::{Deserialize, Serialize};

/// 单个 reasoning effort 的标准化展示信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortInfo {
    /// 推理强度名。
    pub reasoning_effort: String,
    /// 展示描述。
    pub description: String,
}

/// 对外暴露的单个模型目录条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexModelInfo {
    /// 模型唯一 ID。
    pub id: String,
    /// 展示名。
    pub display_name: String,
    /// 描述文本。
    pub description: String,
    /// 是否默认模型。
    pub is_default: bool,
    /// 支持的推理强度列表。
    pub supported_reasoning_efforts: Vec<ReasoningEffortInfo>,
    /// 默认推理强度。
    pub default_reasoning_effort: String,
    /// 输入模态。
    pub input_modalities: Vec<String>,
    /// 输出模态。
    pub output_modalities: Vec<String>,
    /// 是否支持 personality。
    pub supports_personality: bool,
    /// 升级提示。
    pub upgrade: Option<String>,
    /// 来源标记。
    pub source: String,
    /// 当前上下文窗口。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 最大上下文窗口。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    /// 最大输出 token 数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// 截断策略限制。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_policy_limit: Option<u64>,
}
