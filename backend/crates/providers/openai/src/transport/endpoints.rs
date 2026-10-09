//! Codex 上游端点常量、基础地址校验与请求 URL 构造

/// `/codex/responses`
pub const CODEX_RESPONSES_PATH: &str = "/codex/responses";
/// `/codex/images/generations`
pub const CODEX_IMAGE_GENERATIONS_PATH: &str = "/codex/images/generations";
/// `/codex/images/edits`
pub const CODEX_IMAGE_EDITS_PATH: &str = "/codex/images/edits";
/// `/codex/alpha/search`
pub const CODEX_ALPHA_SEARCH_PATH: &str = "/codex/alpha/search";
/// `/codex/realtime/calls`；语音通话 SDP 引导端点。
pub const CODEX_REALTIME_CALLS_PATH: &str = "/codex/realtime/calls";
/// `/api/codex/usage`
pub const CODEX_USAGE_API_PATH: &str = "/api/codex/usage";
/// `/wham/usage`
pub const WHAM_USAGE_PATH: &str = "/wham/usage";
/// Codex Desktop 当前账号个人资料与累计统计
pub const WHAM_PROFILE_STATISTICS_PATH: &str = "/wham/profiles/me";
/// Codex Desktop 主动额度重置卡列表
pub const WHAM_RATE_LIMIT_RESET_CREDITS_PATH: &str = "/wham/rate-limit-reset-credits";
/// Codex Desktop 主动消费额度重置卡
pub const WHAM_RATE_LIMIT_RESET_CREDITS_CONSUME_PATH: &str =
    "/wham/rate-limit-reset-credits/consume";

/// 拼接完整 endpoint URL
pub fn endpoint_url(base_url: &str, endpoint_path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        endpoint_path.trim_start_matches('/')
    )
}

/// 共用上游地址格式校验；启动配置可在解析结果上施加更严格的传输约束
pub(crate) fn parse_upstream_base_url(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    (matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

/// 返回与 base path 对应的唯一 usage endpoint
pub fn usage_endpoint_url(base_url: &str) -> String {
    account_endpoint_url(base_url, "usage")
}

/// Core backend-client 的两个官方路径风格，供所有账号接口共用
pub fn account_endpoint_url(base_url: &str, resource: &str) -> String {
    let prefix = if has_backend_api_base_path(base_url) {
        "wham"
    } else {
        "api/codex"
    };
    endpoint_url(base_url, &format!("{prefix}/{resource}"))
}

fn has_backend_api_base_path(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).ok().is_some_and(|url| {
        url.path()
            .split('/')
            .any(|segment| segment == "backend-api")
    })
}
