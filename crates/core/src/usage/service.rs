//! 用量聚合策略。

use crate::{
    accounts::usage::AccountUsageDelta, protocol::codex::events::TokenUsage,
    usage::model::UsageWindow,
};

/// 用量聚合服务。
#[derive(Debug, Clone, Default)]
pub struct UsageService;

impl UsageService {
    /// 将 token 增量累加到窗口。
    pub fn add_tokens(window: &mut UsageWindow, input_tokens: u64, output_tokens: u64) {
        window.request_count += 1;
        window.input_tokens += input_tokens;
        window.output_tokens += output_tokens;
    }

    /// 将标准化 token 用量转换为账号持久化用量增量。
    pub fn account_delta_from_token_usage(usage: TokenUsage) -> AccountUsageDelta {
        AccountUsageDelta {
            requests: 0,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total_tokens,
            empty_responses: 0,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_requests: 0,
            image_request_failures: 0,
        }
    }
}
