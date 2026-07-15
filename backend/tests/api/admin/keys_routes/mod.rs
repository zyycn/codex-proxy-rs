use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{api::AppState, bootstrap::services::Services};
use serde_json::json;
use sqlx::PgPool;
use tower::util::ServiceExt;

use crate::support::{
    admin::seed_admin_session,
    config::test_config,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
    wire_profile::wire_profile,
};

mod authorization;
mod lifecycle;

async fn admin_client_key_test_app(
    db_name: &str,
) -> (
    axum::Router,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db(db_name).await;
    let redis = create_test_redis(db_name).await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let profile = crate::support::wire_profile::test_wire_profile_value();
    let services = std::sync::Arc::new(Services::new(&config, stores, wire_profile(profile)));
    let state = AppState::from(services.as_ref());
    (
        codex_proxy_rs::api::router::router().with_state(state),
        pool,
        dir,
    )
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
