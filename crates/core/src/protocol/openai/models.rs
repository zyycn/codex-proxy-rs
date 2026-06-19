//! OpenAI 模型列表响应类型。

use serde::{Deserialize, Serialize};

/// OpenAI 模型对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiModel {
    /// 模型 ID。
    pub id: String,
    /// 对象类型。
    pub object: String,
    /// 创建时间戳。
    pub created: i64,
    /// 所有者。
    pub owned_by: String,
}

/// OpenAI 模型列表。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiModelList {
    /// 对象类型。
    pub object: String,
    /// 模型数据。
    pub data: Vec<OpenAiModel>,
}
