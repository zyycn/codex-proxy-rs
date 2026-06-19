//! 服务用量跟踪策略。

use crate::protocol::codex::events::TokenUsage;

/// 判断用量是否为空。
pub fn has_billable_usage(usage: TokenUsage) -> bool {
    usage.total_tokens > 0 || usage.image_input_tokens > 0 || usage.image_output_tokens > 0
}
