use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::usage_record_store::SqliteUsageRecordStore,
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::token_refresh::RefreshLeaseStore,
    upstream::accounts::{cookies::SqliteCookieStore, store::SqliteAccountStore},
    upstream::fingerprint::FingerprintRepository,
};
use tower::util::ServiceExt;

use crate::support::{
    client_keys::insert_client_api_key, config::test_config, http::response_json,
};

#[tokio::test]
async fn models_route_should_reject_unknown_client_api_key() {
    let (app, _key, _dir) = test_app("openai-models-auth.sqlite", false).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer sk_not_stored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn models_route_should_return_uniform_error_for_malformed_authorization() {
    let (app, _key, _dir) = test_app("openai-models-auth-malformed.sqlite", false).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Token sk_not_bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_api_key"
    );
}

#[tokio::test]
async fn models_route_should_accept_stored_client_api_key() {
    let (app, plaintext, _dir) = test_app("openai-models-auth-valid.sqlite", true).await;
    let plaintext = plaintext.expect("client api key should be seeded");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn test_app(
    db_name: &str,
    seed_client_key: bool,
) -> (Router, Option<String>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let plaintext = if seed_client_key {
        Some(insert_client_api_key(&pool).await)
    } else {
        None
    };
    let config = test_config(url);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    services
        .initialize_hot_path_state()
        .await
        .expect("hot path state should initialize");
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

    (app, plaintext, dir)
}
