/// `/codex/responses`
pub const CODEX_RESPONSES_PATH: &str = "/codex/responses";
/// `/codex/images/generations`
pub const CODEX_IMAGE_GENERATIONS_PATH: &str = "/codex/images/generations";
/// `/codex/images/edits`
pub const CODEX_IMAGE_EDITS_PATH: &str = "/codex/images/edits";
/// `/api/codex/usage`
pub const CODEX_USAGE_API_PATH: &str = "/api/codex/usage";
/// `/wham/usage`
pub const WHAM_USAGE_PATH: &str = "/wham/usage";
/// 个人账号每日模型额度权重。
pub const WHAM_DAILY_TOKEN_USAGE_PATH: &str = "/wham/usage/daily-token-usage-breakdown";
/// 个人账号每日 Token 总量。
pub const WHAM_DAILY_WORKSPACE_USAGE_COUNTS_PATH: &str =
    "/wham/analytics/daily-workspace-usage-counts";
/// Codex Desktop 主动额度重置卡列表。
pub const WHAM_RATE_LIMIT_RESET_CREDITS_PATH: &str = "/wham/rate-limit-reset-credits";
/// Codex Desktop 主动消费额度重置卡。
pub const WHAM_RATE_LIMIT_RESET_CREDITS_CONSUME_PATH: &str =
    "/wham/rate-limit-reset-credits/consume";

/// 拼接完整 endpoint URL。
pub fn endpoint_url(base_url: &str, endpoint_path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        endpoint_path.trim_start_matches('/')
    )
}

/// 返回与 base path 对应的唯一 usage endpoint。
pub fn usage_endpoint_url(base_url: &str) -> String {
    let path = if has_backend_api_base_path(base_url) {
        WHAM_USAGE_PATH
    } else {
        CODEX_USAGE_API_PATH
    };
    endpoint_url(base_url, path)
}

fn has_backend_api_base_path(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).ok().is_some_and(|url| {
        url.path()
            .split('/')
            .any(|segment| segment == "backend-api")
    })
}
