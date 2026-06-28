use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use codex_proxy_rs::{config::types::AppConfig, infra::database::connect_sqlite};
use serde_json::Value;
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
    let pool = connect_sqlite(&database_url).await.expect("sqlite pool");
    let config = test_config(database_url);
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
        event_logs: codex_proxy_rs::admin::monitoring::event_store::SqliteEventLogStore::new(
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
    let app = codex_proxy_rs::http::router::router_with_assets(&dist).with_state(state);

    let index = app.clone().oneshot(request("/")).await.expect("index");
    let asset = app
        .clone()
        .oneshot(request("/assets/app.js"))
        .await
        .expect("asset");
    let api = app
        .oneshot(request("/api/admin/settings"))
        .await
        .expect("api response");

    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
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

fn test_config(database_url: String) -> AppConfig {
    let mut config = base_test_config(database_url);
    config.auth.refresh_margin_seconds = 240;
    config.auth.max_concurrent_per_account = 4;
    config.auth.oauth_client_id = "app_id".to_string();
    config.auth.oauth_token_endpoint = "https://auth.invalid/token".to_string();
    config
}
