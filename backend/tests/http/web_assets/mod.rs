use std::path::Path;

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use codex_proxy_rs::{config::types::AppConfig, infra::database::connect_sqlite};
use serde_json::Value;
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::support::{config::test_config as base_test_config, http::response_json};

#[test]
fn asset_headers_should_distinguish_spa_and_fingerprinted_assets() {
    assert_eq!(
        codex_proxy_rs::web::headers::cache_control_for_path("/"),
        "no-cache"
    );
    assert_eq!(
        codex_proxy_rs::web::headers::cache_control_for_path("/assets/app.abc123.js"),
        "public, max-age=31536000, immutable"
    );
}

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
    std::fs::write(dist.join("assets").join("app.js"), "window.__asset = true;")
        .expect("asset should be written");
    let db = dir.path().join("assets-routes.sqlite");
    let database_url = format!("sqlite://{}", db.display());
    let (app, _pool) = router_with_dist_and_database(&dist, &database_url).await;

    let index = app.clone().oneshot(request("/")).await.expect("index");
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
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(health.status(), StatusCode::NO_CONTENT);
    let csp = index
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        csp.contains("style-src 'self';") && !csp.contains("unsafe-inline"),
        "unexpected content security policy: {csp}"
    );
    assert_eq!(
        asset.headers().get(header::CACHE_CONTROL),
        Some(&header::HeaderValue::from_static(
            "public, max-age=31536000, immutable"
        ))
    );
    assert_body_contains(index, "Codex Proxy Admin").await;
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
    let db = dir.path().join("unknown-api-routes.sqlite");
    let database_url = format!("sqlite://{}", db.display());
    let (app, _pool) = router_with_dist_and_database(&dist, &database_url).await;

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
async fn healthz_should_report_unavailable_when_sqlite_is_closed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dist = dir.path().join("dist");
    std::fs::create_dir_all(&dist).expect("dist dir should be created");
    std::fs::write(dist.join("index.html"), "<!doctype html>").expect("index should be written");
    let db = dir.path().join("healthz.sqlite");
    let database_url = format!("sqlite://{}", db.display());
    let (app, pool) = router_with_dist_and_database(&dist, &database_url).await;
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

async fn router_with_dist_and_database(dist: &Path, database_url: &str) -> (Router, SqlitePool) {
    let pool = connect_sqlite(database_url).await.expect("sqlite pool");
    let config = test_config(database_url.to_string());
    let stores = codex_proxy_rs::runtime::services::BackgroundTaskStores {
        accounts: codex_proxy_rs::upstream::accounts::store::SqliteAccountStore::new(pool.clone()),
        admin_sessions: codex_proxy_rs::admin::auth::service::SqliteAdminSessionStore::new(
            pool.clone(),
        ),
        cookies: codex_proxy_rs::upstream::accounts::cookies::SqliteCookieStore::new(pool.clone()),
        fingerprints: codex_proxy_rs::upstream::fingerprint::FingerprintRepository::new(
            pool.clone(),
        ),
        session_affinity:
            codex_proxy_rs::proxy::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                pool.clone(),
            ),
        refresh_leases: codex_proxy_rs::upstream::accounts::token_refresh::RefreshLeaseStore::new(
            pool.clone(),
        ),
        client_keys: codex_proxy_rs::admin::keys::service::SqliteClientKeyStore::new(pool.clone()),
        usage_records:
            codex_proxy_rs::admin::monitoring::usage_record_store::SqliteUsageRecordStore::new(
                pool.clone(),
            ),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(codex_proxy_rs::runtime::services::Services::new(
        &config,
        stores,
        fingerprint,
    ));
    let state = codex_proxy_rs::runtime::state::AppState {
        config,
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router_with_assets(dist).with_state(state),
        pool,
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
