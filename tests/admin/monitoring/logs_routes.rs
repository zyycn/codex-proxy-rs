use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::{
        event_store::SqliteEventLogStore,
        events::{EventLevel, EventLog},
    },
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::token_refresh::RefreshLeaseStore,
    upstream::accounts::{cookies::SqliteCookieStore, store::SqliteAccountStore},
    upstream::fingerprint::FingerprintRepository,
};
use tower::util::ServiceExt;

use crate::support::{admin::seed_admin_session, config::test_config, http::response_json};

#[tokio::test]
async fn admin_logs_should_require_admin_session_cookie() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-auth.sqlite").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs")
                .header("x-request-id", "req_logs_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40101);
}

#[tokio::test]
async fn admin_logs_should_cursor_page_events_and_include_request_id() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs-cursor.sqlite").await;
    let now = Utc::now();
    let mut older = EventLog::new("request", EventLevel::Info, "older");
    older.id = "log_older".to_string();
    older.created_at = now;
    store.append(&older).await.unwrap();
    let mut newer = EventLog::new("request", EventLevel::Info, "newer");
    newer.id = "log_newer".to_string();
    newer.created_at = now + Duration::seconds(1);
    store.append(&newer).await.unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_cursor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["code"], 200);
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn admin_logs_should_return_numbered_page_metadata() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs-numbered.sqlite").await;
    let now = Utc::now();
    for (id, message, offset) in [
        ("log_old", "older timeout", 0),
        ("log_new", "newer timeout", 1),
    ] {
        let mut event = EventLog::new("request", EventLevel::Error, message);
        event.id = id.to_string();
        event.created_at = now + Duration::seconds(offset);
        store.append(&event).await.unwrap();
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?page=1&pageSize=1&level=error&search=timeout")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_numbered")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["items"][0]["id"], "log_new");
    assert_eq!(body["data"]["page"]["page"], 1);
    assert_eq!(body["data"]["page"]["pageSize"], 1);
    assert_eq!(body["data"]["page"]["total"], 2);
    assert_eq!(body["data"]["page"]["totalPages"], 2);
}

#[tokio::test]
async fn admin_logs_should_filter_and_cursor_page_events() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs.sqlite").await;
    let mut matching = EventLog::new("request", EventLevel::Error, "upstream timeout");
    matching.id = "log_matching".to_string();
    matching.route = Some("/v1/responses".to_string());
    store.append(&matching).await.unwrap();
    store
        .append(&EventLog::new(
            "request",
            EventLevel::Info,
            "upstream timeout",
        ))
        .await
        .unwrap();
    store
        .append(&EventLog::new(
            "account",
            EventLevel::Error,
            "upstream timeout",
        ))
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?kind=request&level=error&search=timeout&limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_filter")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["data"]["items"][0]["id"], "log_matching");
}

#[tokio::test]
async fn admin_logs_should_reject_unsupported_level_filter() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-invalid-level.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?level=verbose")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_invalid_level")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_logs_should_return_detail_and_clear_events() {
    let (app, store, _dir) = admin_logs_test_app("admin-logs-state.sqlite").await;
    let mut event = EventLog::new("request", EventLevel::Warn, "detail");
    event.id = "log_detail".to_string();
    event.request_id = Some("req_upstream".to_string());
    store.append(&event).await.unwrap();

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/detail?id=log_detail")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);

    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/logs/delete")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response_json(cleared).await["data"]["cleared"], 1);

    let empty = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs?limit=50")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(empty).await["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn admin_logs_detail_should_return_not_found_for_missing_event() {
    let (app, _store, _dir) = admin_logs_test_app("admin-logs-detail-missing.sqlite").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/logs/detail?id=missing")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_logs_missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn admin_logs_test_app(
    db_name: &str,
) -> (axum::Router, SqliteEventLogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        SqliteEventLogStore::new(pool),
        dir,
    )
}
