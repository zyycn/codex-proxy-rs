use provider_openai::transport::{endpoint_request_path, endpoint_url, usage_endpoint_urls};

#[test]
fn endpoints_should_join_origin_and_backend_paths_without_double_slashes() {
    assert_eq!(
        (
            endpoint_url("https://api.example.com/", "/codex/responses"),
            endpoint_request_path("https://api.example.com/backend-api", "/codex/usage"),
        ),
        (
            "https://api.example.com/codex/responses".to_owned(),
            "/backend-api/codex/usage".to_owned(),
        )
    );
}

#[test]
fn usage_endpoints_should_preserve_the_official_backend_api_fallback_order() {
    assert_eq!(
        usage_endpoint_urls("https://chatgpt.com/backend-api"),
        vec![
            "https://chatgpt.com/backend-api/wham/usage".to_owned(),
            "https://chatgpt.com/backend-api/codex/usage".to_owned(),
        ]
    );
}
