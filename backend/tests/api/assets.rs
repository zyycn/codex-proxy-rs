use std::path::Path;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use codex_proxy_rs::bootstrap::config::AppConfig;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

use crate::support::{
    config::test_config as base_test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{
        TestDatabaseGuard, background_task_stores, create_test_redis, init_test_db,
        test_database_url,
    },
};

#[tokio::test]
async fn server_router_should_serve_frontend_assets_without_shadowing_api_routes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dist = dir.path().join("dist");
    std::fs::create_dir_all(dist.join("assets")).expect("assets dir should be created");
    std::fs::write(
        dist.join("index.html"),
        "<!doctype html><main id=\"app\">Codex Proxy Admin</main>",
    )
    .expect("index should be written");
    std::fs::write(
        dist.join("favicon.svg"),
        "<svg><title>Codex Proxy</title></svg>",
    )
    .expect("favicon should be written");
    std::fs::write(dist.join("assets").join("app.js"), "window.__asset = true;")
        .expect("asset should be written");
    let (app, _pool, _guard) = router_with_dist_and_database(&dist, "assets-routes").await;

    let index = app.clone().oneshot(request("/")).await.expect("index");
    let route_fallback = app
        .clone()
        .oneshot(request("/dashboard"))
        .await
        .expect("route fallback");
    let favicon = app
        .clone()
        .oneshot(request("/favicon.svg"))
        .await
        .expect("favicon");
    let missing_static = app
        .clone()
        .oneshot(request("/missing.svg"))
        .await
        .expect("missing static response");
    let asset = app
        .clone()
        .oneshot(request("/assets/app.js"))
        .await
        .expect("asset");
    let api = app
        .clone()
        .oneshot(request("/api/admin/settings"))
        .await
        .expect("api response");
    let health = app
        .oneshot(request("/healthz"))
        .await
        .expect("health response");

    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(route_fallback.status(), StatusCode::OK);
    assert_eq!(favicon.status(), StatusCode::OK);
    assert_eq!(missing_static.status(), StatusCode::NOT_FOUND);
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(health.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        favicon.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("image/svg+xml"))
    );
    assert_no_static_policy_headers(&index);
    assert_no_static_policy_headers(&route_fallback);
    assert_no_static_policy_headers(&favicon);
    assert_no_static_policy_headers(&asset);
    assert_body_contains(index, "Codex Proxy Admin").await;
    assert_body_contains(route_fallback, "Codex Proxy Admin").await;
    assert_body_contains(favicon, "Codex Proxy").await;
    assert_body_contains(asset, "__asset").await;
    assert_eq!(response_json(api).await["code"], Value::from(40101));
}

#[tokio::test]
async fn server_router_should_return_json_404_for_unknown_api_routes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dist = dir.path().join("dist");
    std::fs::create_dir_all(&dist).expect("dist dir should be created");
    std::fs::write(
        dist.join("index.html"),
        "<!doctype html><main id=\"app\">Codex Proxy Admin</main>",
    )
    .expect("index should be written");
    let (app, _pool, _guard) = router_with_dist_and_database(&dist, "unknown-api-routes").await;

    let admin = app
        .clone()
        .oneshot(request("/api/admin/does-not-exist"))
        .await
        .expect("admin unknown response");
    let openai = app
        .oneshot(request("/v1/does-not-exist"))
        .await
        .expect("openai unknown response");

    assert_unknown_api_route(admin).await;
    assert_unknown_api_route(openai).await;
}

#[tokio::test]
async fn healthz_should_report_unavailable_when_postgres_is_closed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dist = dir.path().join("dist");
    std::fs::create_dir_all(&dist).expect("dist dir should be created");
    std::fs::write(dist.join("index.html"), "<!doctype html>").expect("index should be written");
    let (app, pool, _guard) = router_with_dist_and_database(&dist, "healthz-postgres").await;
    pool.close().await;

    let response = app
        .oneshot(request("/healthz"))
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

fn request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request should build")
}

async fn assert_body_contains(response: axum::response::Response, expected: &str) {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should collect");
    let text = std::str::from_utf8(&bytes).expect("body should be utf8");

    assert!(
        text.contains(expected),
        "body should contain `{expected}`, got `{text}`"
    );
}

async fn assert_unknown_api_route(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_json(response).await;
    assert_eq!(body["code"], Value::from(40401));
    assert_eq!(body["message"], Value::from("API route not found"));
}

fn assert_no_static_policy_headers(response: &axum::response::Response) {
    assert!(!response.headers().contains_key(header::CACHE_CONTROL));
    assert!(
        !response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    );
    assert!(
        !response
            .headers()
            .contains_key(header::X_CONTENT_TYPE_OPTIONS)
    );
    assert!(!response.headers().contains_key("x-frame-options"));
    assert!(!response.headers().contains_key("referrer-policy"));
}

async fn router_with_dist_and_database(
    dist: &Path,
    label: &str,
) -> (Router, PgPool, TestDatabaseGuard) {
    let (pool, guard) = init_test_db(label).await;
    let redis = create_test_redis(label).await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(codex_proxy_rs::bootstrap::services::Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = codex_proxy_rs::api::AppState::from(services.as_ref());
    (
        codex_proxy_rs::api::router::router_with_assets(dist).with_state(state),
        pool,
        guard,
    )
}

fn test_config(database_url: String) -> AppConfig {
    let mut config = base_test_config(database_url);
    config.auth.refresh_margin_seconds = 240;
    config.auth.max_concurrent_per_account = 4;
    config.auth.oauth_client_id = "app_id".to_string();
    config.auth.oauth_token_endpoint = "https://auth.invalid/token".to_string();
    config
}
