//! Codex Chat 扩展。

use serde::{Deserialize, Serialize};

/// Codex chat 侧的扩展选项。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexChatOptions {
    /// 是否包含 reasoning 摘要。
    pub include_reasoning: bool,
}
