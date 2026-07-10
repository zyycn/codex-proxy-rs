//! 上游模型目录原始条目。

use serde::{Deserialize, Serialize};

/// 上游声明的 reasoning effort 条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendReasoningEffort {
    /// reasoning_effort 字段。
    #[serde(default, rename = "reasoning_effort", alias = "reasoningEffort")]
    pub reasoning_effort: Option<String>,
    /// 上游可能返回的 effort 字段别名。
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
