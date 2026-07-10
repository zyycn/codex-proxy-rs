//! 模型目录配置。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 模型目录别名配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    /// 模型别名映射。
    pub model_aliases: BTreeMap<String, String>,
}
