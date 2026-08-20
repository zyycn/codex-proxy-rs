/// `/codex/responses`
pub const CODEX_RESPONSES_PATH: &str = "/codex/responses";
/// `/images/generations`
pub const CODEX_IMAGE_GENERATIONS_PATH: &str = "/images/generations";
/// `/images/edits`
pub const CODEX_IMAGE_EDITS_PATH: &str = "/images/edits";
/// `/api/codex/usage`
pub const CODEX_USAGE_API_PATH: &str = "/api/codex/usage";
/// `/wham/usage`
pub const WHAM_USAGE_PATH: &str = "/wham/usage";

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
