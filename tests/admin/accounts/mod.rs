use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::event_store::SqliteEventLogStore,
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::{
        cookies::SqliteCookieStore,
        model::AccountStatus,
        store::{NewAccount, SqliteAccountStore},
        token_refresh::RefreshLeaseStore,
    },
    upstream::fingerprint::FingerprintRepository,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

use crate::support::jwt::unsigned_jwt;
use crate::support::{admin::seed_admin_session, config::test_config, http::response_json};

mod exporting;
mod importing;
mod lifecycle;
mod list;
mod oauth;
mod quota;
mod testing;

struct UsageAccountSeed<'a> {
    id: &'a str,
    email: &'a str,
    label: &'a str,
    plan_type: &'a str,
    request_count: i64,
    empty_response_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    last_used_at: &'a str,
}

async fn seed_usage_account(pool: &SqlitePool, seed: UsageAccountSeed<'_>) {
    sqlx::query("insert into accounts (id, email, label, plan_type, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, 'active', ?, ?)")
        .bind(seed.id).bind(seed.email).bind(seed.label).bind(seed.plan_type).bind("access-token")
        .bind("2026-06-11T00:00:00Z").bind("2026-06-11T00:00:00Z")
        .execute(pool).await.unwrap();
    sqlx::query("insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)")
        .bind(seed.id)
        .bind(seed.request_count)
        .bind(seed.empty_response_count)
        .bind(seed.input_tokens)
        .bind(seed.output_tokens)
        .bind(seed.cached_tokens)
        .bind(seed.last_used_at)
        .execute(pool).await.unwrap();
}

async fn post_admin_account(app: &axum::Router, payload: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn admin_accounts_test_app(
    db_name: &str,
    key_byte: u8,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir) {
    admin_accounts_test_app_with_api_base_url(
        db_name,
        key_byte,
        "https://chatgpt.com/backend-api".to_string(),
    )
    .await
}

async fn admin_accounts_test_app_with_api_base_url(
    db_name: &str,
    key_byte: u8,
    api_base_url: String,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir) {
    admin_accounts_test_app_with_overrides(db_name, key_byte, api_base_url, None).await
}

async fn admin_accounts_test_app_with_oauth_token_endpoint(
    db_name: &str,
    key_byte: u8,
    oauth_token_endpoint: String,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir) {
    admin_accounts_test_app_with_overrides(
        db_name,
        key_byte,
        "https://chatgpt.com/backend-api".to_string(),
        Some(oauth_token_endpoint),
    )
    .await
}

async fn admin_accounts_test_app_with_api_base_url_and_oauth_token_endpoint(
    db_name: &str,
    key_byte: u8,
    api_base_url: String,
    oauth_token_endpoint: String,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir) {
    admin_accounts_test_app_with_overrides(
        db_name,
        key_byte,
        api_base_url,
        Some(oauth_token_endpoint),
    )
    .await
}

async fn admin_accounts_test_app_with_overrides(
    db_name: &str,
    _key_byte: u8,
    api_base_url: String,
    oauth_token_endpoint: Option<String>,
) -> (axum::Router, AppState, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let mut config = test_config(url);
    config.api.base_url = api_base_url;
    if let Some(oauth_token_endpoint) = oauth_token_endpoint {
        config.auth.oauth_token_endpoint = oauth_token_endpoint;
    }
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
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state.clone());
    (app, state, pool, dir)
}

async fn seed_account(pool: &SqlitePool, account: NewAccount) {
    SqliteAccountStore::new(pool.clone())
        .insert(account)
        .await
        .unwrap();
}

fn test_jwt(
    account_id: &str,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
) -> String {
    test_jwt_with_exp(Some(account_id), user_id, email, plan_type, 4_102_444_800)
}

fn test_jwt_with_exp(
    account_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
    exp: i64,
) -> String {
    let payload = json!({
        "exp": exp,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": user_id,
            "chatgpt_plan_type": plan_type,
        },
        "https://api.openai.com/profile": { "email": email }
    });
    unsigned_jwt(&payload)
}
