use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use tower::ServiceExt;

#[test]
fn asset_headers_should_distinguish_spa_and_fingerprinted_assets() {
    assert_eq!(
        codex_proxy_assets::headers::cache_control_for_path("/"),
        "no-cache"
    );
    assert_eq!(
        codex_proxy_assets::headers::cache_control_for_path("/assets/app.abc123.js"),
        "public, max-age=31536000, immutable"
    );
}

#[tokio::test]
async fn asset_router_should_serve_index_and_spa_fallback() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><main id=\"app\">Codex Proxy</main>",
    )
    .expect("index should be written");
    std::fs::create_dir(dir.path().join("assets")).expect("assets dir should be created");

    let router = codex_proxy_assets::router::spa_router(dir.path());

    let index = router
        .clone()
        .oneshot(request("/"))
        .await
        .expect("index response");
    let fallback = router
        .oneshot(request("/admin/accounts"))
        .await
        .expect("fallback response");

    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(fallback.status(), StatusCode::OK);
    assert_body_contains(index, "Codex Proxy").await;
    assert_body_contains(fallback, "Codex Proxy").await;
}

#[tokio::test]
async fn asset_router_should_serve_assets_with_cache_and_security_headers() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("index.html"), "<!doctype html>").expect("write index");
    std::fs::create_dir(dir.path().join("assets")).expect("assets dir should be created");
    std::fs::write(
        dir.path().join("assets").join("app.abc123.js"),
        "window.__codexProxy = true;",
    )
    .expect("asset should be written");

    let response = codex_proxy_assets::router::spa_router(dir.path())
        .oneshot(request("/assets/app.abc123.js"))
        .await
        .expect("asset response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&header::HeaderValue::from_static(
            "public, max-age=31536000, immutable"
        ))
    );
    assert_eq!(
        response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
        Some(&header::HeaderValue::from_static("nosniff"))
    );
    assert_body_contains(response, "__codexProxy").await;
}

fn request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request should build")
}

async fn assert_body_contains(response: axum::response::Response, expected: &str) {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should collect");
    let text = std::str::from_utf8(&body).expect("body should be utf8");

    assert!(
        text.contains(expected),
        "body should contain `{expected}`, got `{text}`"
    );
}
