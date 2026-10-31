//! 固定 Provider 的直接别名与受管适配模型目录合同

use serde::{Deserialize, Serialize};

/// 指向内置 Provider 上游模型的直接别名，不创建新的 Provider 执行器
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAlias {
    pub id: String,
    pub provider: String,
    pub model: String,
}

/// v2 同时发布直接别名与受管适配器实际提供的模型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogRegistrationV2 {
    pub aliases: Vec<ModelAlias>,
    pub models: Vec<AdaptedModel>,
}

/// 账号与 Provider 取自同实例的 adapter 声明，不另复制地址或凭据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptedModel {
    pub id: String,
    pub adapter_id: String,
    pub features: Vec<super::middleware::RequestFeature>,
    pub max_output_tokens: Option<u64>,
    pub presentation: ModelPresentation,
}

/// 通用客户端画像；省略的能力不向客户端宣告支持
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelPresentation {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub default_reasoning_effort: Option<String>,
    pub supported_reasoning_efforts: Vec<String>,
    pub reasoning_summary: bool,
    pub context_window_tokens: Option<u64>,
    pub max_context_window_tokens: Option<u64>,
    pub image_input: bool,
    pub agent_tools: bool,
    pub parallel_tool_calls: bool,
    pub search_tool: bool,
    pub image_detail_original: bool,
    pub verbosity: bool,
    pub hidden: bool,
}
