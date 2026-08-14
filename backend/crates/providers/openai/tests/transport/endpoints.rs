use provider_openai::transport::{endpoint_url, usage_endpoint_url};

#[test]
fn endpoints_should_join_origin_and_backend_paths_without_double_slashes() {
    assert_eq!(
        endpoint_url("https://api.example.com/", "/codex/responses"),
        "https://api.example.com/codex/responses"
    );
}

#[test]
fn usage_endpoint_should_use_the_official_wham_path_for_backend_api() {
    assert_eq!(
        usage_endpoint_url("https://chatgpt.com/backend-api"),
        "https://chatgpt.com/backend-api/wham/usage"
    );
}
