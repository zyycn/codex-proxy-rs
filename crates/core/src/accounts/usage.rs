//! 账号用量累积策略。

/// 账号用量增量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUsageDelta {
    /// 请求数增量。
    pub requests: u64,
    /// 输入 token 增量。
    pub input_tokens: u64,
    /// 输出 token 增量。
    pub output_tokens: u64,
    /// 缓存 token 增量。
    pub cached_tokens: u64,
    /// 空响应数增量。
    pub empty_responses: u64,
    /// 图片工具输入 token 增量。
    pub image_input_tokens: u64,
    /// 图片工具输出 token 增量。
    pub image_output_tokens: u64,
    /// 图片工具成功请求数增量。
    pub image_requests: u64,
    /// 图片工具失败请求数增量。
    pub image_request_failures: u64,
}

impl AccountUsageDelta {
    /// 合并两个用量增量。
    pub fn merged(self, other: Self) -> Self {
        Self {
            requests: self.requests + other.requests,
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cached_tokens: self.cached_tokens + other.cached_tokens,
            empty_responses: self.empty_responses + other.empty_responses,
            image_input_tokens: self.image_input_tokens + other.image_input_tokens,
            image_output_tokens: self.image_output_tokens + other.image_output_tokens,
            image_requests: self.image_requests + other.image_requests,
            image_request_failures: self.image_request_failures + other.image_request_failures,
        }
    }
}
