/// `/codex/responses`
pub const CODEX_RESPONSES_PATH: &str = "/codex/responses";
/// `/codex/responses/compact`
pub const CODEX_RESPONSES_COMPACT_PATH: &str = "/codex/responses/compact";
/// `/codex/usage`
pub const CODEX_USAGE_PATH: &str = "/codex/usage";
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

/// 计算请求层路径。
pub fn endpoint_request_path(base_url: &str, endpoint_path: &str) -> String {
    let endpoint_path = endpoint_path.trim_start_matches('/');
    let base_path = reqwest::Url::parse(base_url)
        .ok()
        .map(|url| url.path().trim_end_matches('/').to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_default();

    if base_path.is_empty() {
        format!("/{endpoint_path}")
    } else {
        format!("{base_path}/{endpoint_path}")
    }
}

/// 返回 usage 相关 endpoint 的完整 URL 列表。
pub fn usage_endpoint_urls(base_url: &str) -> Vec<String> {
    usage_endpoint_paths(base_url)
        .into_iter()
        .map(|path| endpoint_url(base_url, path))
        .collect()
}

/// 返回 usage 主请求路径。
pub fn primary_usage_request_path(base_url: &str) -> String {
    let endpoint_path = usage_endpoint_paths(base_url)
        .into_iter()
        .next()
        .unwrap_or(CODEX_USAGE_API_PATH);
    endpoint_request_path(base_url, endpoint_path)
}

fn usage_endpoint_paths(base_url: &str) -> Vec<&'static str> {
    if has_backend_api_base_path(base_url) {
        vec![WHAM_USAGE_PATH, CODEX_USAGE_PATH]
    } else {
        vec![CODEX_USAGE_API_PATH, CODEX_USAGE_PATH]
    }
}

fn has_backend_api_base_path(base_url: &str) -> bool {
    reqwest::Url::parse(base_url).ok().is_some_and(|url| {
        url.path()
            .split('/')
            .any(|segment| segment == "backend-api")
    })
}
