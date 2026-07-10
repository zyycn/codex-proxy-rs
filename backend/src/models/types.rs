//! 模型目录领域类型。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 模型目录别名配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    /// 模型别名映射。
    pub model_aliases: BTreeMap<String, String>,
}

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

/// 按计划类型持久化的模型快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPlanSnapshot {
    /// 订阅计划类型。
    pub plan_type: String,
    /// 该计划可见的模型列表。
    pub models: Vec<CodexModelInfo>,
}

/// 上游声明的 reasoning effort 条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendReasoningEffort {
    #[serde(default, rename = "reasoning_effort", alias = "reasoningEffort")]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// 上游截断策略定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendTruncationPolicy {
    pub limit: Option<u64>,
}

/// 上游模型目录的原始模型条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendModelEntry {
    pub slug: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    pub default_reasoning_effort: Option<String>,
    pub default_reasoning_level: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<BackendReasoningEffort>,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<BackendReasoningEffort>,
    pub input_modalities: Option<Vec<String>>,
    pub output_modalities: Option<Vec<String>>,
    pub supports_personality: Option<bool>,
    pub upgrade: Option<String>,
    #[serde(alias = "contextWindow")]
    pub context_window: Option<u64>,
    #[serde(alias = "maxContextWindow")]
    pub max_context_window: Option<u64>,
    #[serde(alias = "maxOutputTokens")]
    pub max_output_tokens: Option<u64>,
    #[serde(alias = "truncationPolicy")]
    pub truncation_policy: Option<BackendTruncationPolicy>,
}
