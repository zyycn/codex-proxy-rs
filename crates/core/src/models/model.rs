//! 模型目录领域中的纯数据类型。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 模型目录默认值与别名配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    /// 默认模型名。
    pub default_model: String,
    /// 默认推理强度。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    pub service_tier: Option<String>,
    /// 模型别名映射。
    pub aliases: BTreeMap<String, String>,
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

/// 解析模型名后得到的标准化结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelName {
    /// 最终模型 ID。
    pub model_id: String,
    /// 推理强度后缀。
    pub reasoning_effort: Option<String>,
    /// 服务层级后缀。
    pub service_tier: Option<String>,
}

/// 模型目录调试视图。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelStoreDebug {
    /// 模型总数。
    pub total_models: usize,
    /// 静态模型数。
    pub static_models: usize,
    /// 别名数量。
    pub alias_count: usize,
    /// 调试模型条目。
    pub models: Vec<ModelDebugEntry>,
}

/// 单个调试模型条目。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelDebugEntry {
    /// 模型 ID。
    pub id: String,
    /// 来源标记。
    pub source: String,
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
    /// reasoning_effort 字段。
    #[serde(default, rename = "reasoning_effort", alias = "reasoningEffort")]
    pub reasoning_effort: Option<String>,
    /// effort 字段的兼容别名。
    #[serde(default)]
    pub effort: Option<String>,
    /// 展示描述。
    #[serde(default)]
    pub description: Option<String>,
}

/// 上游截断策略定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendTruncationPolicy {
    /// 上下文截断限制。
    pub limit: Option<u64>,
}

/// 上游 `/codex/models` 返回的原始模型条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendModelEntry {
    /// slug。
    pub slug: Option<String>,
    /// id。
    pub id: Option<String>,
    /// name。
    pub name: Option<String>,
    /// display_name。
    pub display_name: Option<String>,
    /// title。
    pub title: Option<String>,
    /// description。
    pub description: Option<String>,
    /// 是否默认模型。
    pub is_default: Option<bool>,
    /// 默认 reasoning effort。
    pub default_reasoning_effort: Option<String>,
    /// 默认 reasoning level。
    pub default_reasoning_level: Option<String>,
    /// 支持的 reasoning effort 列表。
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<BackendReasoningEffort>,
    /// 支持的 reasoning level 列表。
    #[serde(default)]
    pub supported_reasoning_levels: Vec<BackendReasoningEffort>,
    /// 输入模态。
    pub input_modalities: Option<Vec<String>>,
    /// 输出模态。
    pub output_modalities: Option<Vec<String>>,
    /// 是否支持 personality。
    pub supports_personality: Option<bool>,
    /// 升级提示。
    pub upgrade: Option<String>,
    /// contextWindow 别名。
    #[serde(alias = "contextWindow")]
    pub context_window: Option<u64>,
    /// maxContextWindow 别名。
    #[serde(alias = "maxContextWindow")]
    pub max_context_window: Option<u64>,
    /// maxOutputTokens 别名。
    #[serde(alias = "maxOutputTokens")]
    pub max_output_tokens: Option<u64>,
    /// truncationPolicy 别名。
    #[serde(alias = "truncationPolicy")]
    pub truncation_policy: Option<BackendTruncationPolicy>,
}
