//! 固定 Provider 的模型别名贡献；目标功能与协议元数据由宿主目录提供。

use serde::{Deserialize, Serialize};

/// 一次实例准备产生的完整目录，随实例配置原子发布。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogRegistration {
    pub models: Vec<ModelAlias>,
}

/// 指向内置 Provider 上游模型的直接别名，不创建新的 Provider 执行器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAlias {
    pub id: String,
    pub provider: String,
    pub model: String,
}
