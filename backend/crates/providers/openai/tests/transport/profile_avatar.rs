//! 验证头像请求的官方来源限制、认证边界与流式交付

use futures::TryStreamExt as _;
use provider_openai::transport::{
    CodexProfileAvatarFetchError, CodexRequestContext, fetch_profile_avatar,
    profile_avatar::build_profile_avatar_request,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OFFICIAL_AVATAR_SOURCE: &str =
    "https://chatgpt.com/backend-api/estuary/public_content/enc/opaque-token=";

#[test]
fn auth0_default_avatar_uses_the_public_origin_without_account_credentials() {
    let source = "https://cdn.auth0.com/avatars/te.png";
    let mut context = CodexRequestContext::auxiliary(
        "Bearer private-token",
        Some("private-account"),
        "avatar",
        None,
    );
    context.cookie_header = Some("private-cookie=value");
    let request = build_profile_avatar_request(
        &reqwest::Client::new(),
        "http://127.0.0.1:9/backend-api",
        &super::test_wire_profile().snapshot(),
        source,
        context,
    )
    .expect("public avatar request");
    assert_eq!(request.url().as_str(), source);
    for name in [
        "authorization",
        "chatgpt-account-id",
        "cookie",
        "originator",
        "version",
    ] {
        assert!(!request.headers().contains_key(name), "public CDN: {name}");
    }
}

#[tokio::test]
async fn profile_avatar_streams_unrestricted_content_type_and_body_size() {
    let server = MockServer::start().await;
    let body = vec![b'x'; 1024 * 1024 + 1];
    Mock::given(method("GET"))
        .and(path("/estuary/public_content/enc/opaque-token="))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/x-avatar-fixture")
                .insert_header("etag", "\"avatar-v1\"")
                .set_body_bytes(body.clone()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");

    let avatar = fetch_profile_avatar(
        &client,
        &server.uri(),
        &super::test_wire_profile().snapshot(),
        OFFICIAL_AVATAR_SOURCE,
        CodexRequestContext::auxiliary(
            "Bearer avatar-token",
            Some("avatar-account"),
            "avatar",
            None,
        ),
    )
    .await
    .expect("profile avatar");
    let content_type = avatar.content_type.clone();
    let content_length = avatar.content_length;
    let etag = avatar.etag.clone();
    let chunks = avatar.body.try_collect::<Vec<_>>().await.expect("body");
    let streamed = chunks.into_iter().flatten().collect::<Vec<_>>();
    let requests = server.received_requests().await.expect("avatar request");
    let headers = &requests.first().expect("one avatar request").headers;

    assert_eq!(
        content_type.as_deref(),
        Some("application/x-avatar-fixture")
    );
    assert_eq!(content_length, Some(body.len() as u64));
    assert_eq!(etag.as_deref(), Some("\"avatar-v1\""));
    assert_eq!(streamed, body);
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("*/*")
    );
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs/1.2.3 (linux; x86_64)")
    );
    assert_eq!(headers["authorization"], "Bearer avatar-token");
    assert_eq!(headers["chatgpt-account-id"], "avatar-account");
    assert_eq!(headers["originator"], "codex_cli_rs");
    assert!(!headers.contains_key("version"));
}

#[tokio::test]
async fn profile_avatar_rejects_non_official_sources_before_request() {
    let client = reqwest::Client::new();

    for source in [
        "https://example.com/backend-api/estuary/public_content/enc/token",
        "https://chatgpt.com/other/token",
        "https://chatgpt.com/backend-api/estuary/public_content/enc/token?next=1",
        "https://chatgpt.com/backend-api/estuary/public_content/enc/",
        "http://cdn.auth0.com/avatars/te.png",
        "https://cdn.auth0.com:444/avatars/te.png",
        "https://cdn.auth0.com.evil.test/avatars/te.png",
        "https://cdn.auth0.com@evil.test/avatars/te.png",
        "https://user:password@cdn.auth0.com/avatars/te.png",
        "https://cdn.auth0.com/other/te.png",
        "https://cdn.auth0.com/avatars/",
        "https://cdn.auth0.com/avatars/te.png?next=1",
        "https://cdn.auth0.com/avatars/te.png#fragment",
    ] {
        let error = fetch_profile_avatar(
            &client,
            "http://127.0.0.1:9",
            &super::test_wire_profile().snapshot(),
            source,
            CodexRequestContext::auxiliary(
                "Bearer avatar-token",
                Some("avatar-account"),
                "avatar",
                None,
            ),
        )
        .await
        .expect_err("invalid source");
        assert!(matches!(error, CodexProfileAvatarFetchError::InvalidSource));
    }
}
