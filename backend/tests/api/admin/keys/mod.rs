use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{bootstrap::services::Services, bootstrap::state::AppState};
use serde_json::json;
use tower::util::ServiceExt;

use crate::support::{
    admin::seed_admin_session,
    config::test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

mod authorization;
mod lifecycle;
mod store;

async fn admin_client_key_test_app(db_name: &str) -> (axum::Router, tempfile::TempDir) {
    let (pool, dir) = init_test_db(db_name).await;
    let redis = create_test_redis(db_name).await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState {
        services: (*services).clone(),
    };
    (codex_proxy_rs::api::router::router().with_state(state), dir)
}

async fn create_admin_client_key(app: &axum::Router, name: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    (
        body["data"]["id"].as_str().unwrap().to_string(),
        body["data"]["key"].as_str().unwrap().to_string(),
    )
}
