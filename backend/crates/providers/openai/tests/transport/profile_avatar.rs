use futures::TryStreamExt as _;
use provider_openai::transport::{CodexProfileAvatarFetchError, fetch_profile_avatar};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OFFICIAL_AVATAR_SOURCE: &str =
    "https://chatgpt.com/backend-api/estuary/public_content/enc/opaque-token=";

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
        "codex_cli_rs/1.0.0 (linux; x86_64)",
        OFFICIAL_AVATAR_SOURCE,
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
        Some("image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8")
    );
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs/1.0.0 (linux; x86_64)")
    );
}

#[tokio::test]
async fn profile_avatar_rejects_non_official_sources_before_request() {
    let client = reqwest::Client::new();

    for source in [
        "https://example.com/backend-api/estuary/public_content/enc/token",
        "https://chatgpt.com/other/token",
        "https://chatgpt.com/backend-api/estuary/public_content/enc/token?next=1",
        "https://chatgpt.com/backend-api/estuary/public_content/enc/",
    ] {
        let error = fetch_profile_avatar(&client, "http://127.0.0.1:9", "test-agent", source)
            .await
            .expect_err("invalid source");
        assert!(matches!(error, CodexProfileAvatarFetchError::InvalidSource));
    }
}
