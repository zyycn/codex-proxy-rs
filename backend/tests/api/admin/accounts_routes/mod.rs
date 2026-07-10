use std::{ops::Deref, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    api::AppState,
    bootstrap::services::Services,
    fleet::{
        account::AccountStatus,
        cookies::PgCookieStore,
        store::{NewAccount, PgAccountStore},
    },
    infra::redis::RedisConnection,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::util::ServiceExt;

use crate::support::jwt::unsigned_jwt;
use crate::support::{
    admin::seed_admin_session,
    config::test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

mod export_routes;
mod import_routes;
mod lifecycle_routes;
mod list;
mod oauth_routes;
mod probe_routes;
mod quota_routes;

struct AdminAccountsTestState {
    app_state: AppState,
    redis: RedisConnection,
}

impl Deref for AdminAccountsTestState {
    type Target = AppState;

    fn deref(&self) -> &Self::Target {
        &self.app_state
    }
}

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
    window_request_count: i64,
    window_input_tokens: i64,
    window_output_tokens: i64,
    window_cached_tokens: i64,
    window_started_at: &'a str,
    window_reset_at: &'a str,
    limit_window_seconds: i64,
    last_used_at: &'a str,
}

async fn seed_usage_account(pool: &PgPool, seed: UsageAccountSeed<'_>) {
    sqlx::query("insert into accounts (id, email, label, plan_type, access_token, status, added_at, updated_at) values ($1, $2, $3, $4, $5, 'active', $6, $7)")
        .bind(seed.id).bind(seed.email).bind(seed.label).bind(seed.plan_type).bind("access-token")
        .bind(crate::support::storage::timestamp("2026-06-11T00:00:00Z")).bind(crate::support::storage::timestamp("2026-06-11T00:00:00Z"))
        .execute(pool).await.unwrap();
    sqlx::query("insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_started_at, window_reset_at, limit_window_seconds, last_used_at) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)")
        .bind(seed.id)
        .bind(seed.request_count)
        .bind(seed.empty_response_count)
        .bind(seed.input_tokens)
        .bind(seed.output_tokens)
        .bind(seed.cached_tokens)
        .bind(seed.window_request_count)
        .bind(seed.window_input_tokens)
        .bind(seed.window_output_tokens)
        .bind(seed.window_cached_tokens)
        .bind(crate::support::storage::timestamp(seed.window_started_at))
        .bind(crate::support::storage::timestamp(seed.window_reset_at))
        .bind(seed.limit_window_seconds)
        .bind(crate::support::storage::timestamp(seed.last_used_at))
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
) -> (
    axum::Router,
    AdminAccountsTestState,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
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
) -> (
    axum::Router,
    AdminAccountsTestState,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    admin_accounts_test_app_with_overrides(db_name, key_byte, api_base_url, None).await
}

async fn admin_accounts_test_app_with_oauth_token_endpoint(
    db_name: &str,
    key_byte: u8,
    oauth_token_endpoint: String,
) -> (
    axum::Router,
    AdminAccountsTestState,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
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
) -> (
    axum::Router,
    AdminAccountsTestState,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
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
) -> (
    axum::Router,
    AdminAccountsTestState,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db(db_name).await;
    let redis = create_test_redis(db_name).await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let mut config = test_config(test_database_url());
    config.api.base_url = api_base_url;
    if let Some(oauth_token_endpoint) = oauth_token_endpoint {
        config.auth.oauth_token_endpoint = oauth_token_endpoint;
    }
    let stores = background_task_stores(pool.clone(), redis.clone());
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state.clone());
    (
        app,
        AdminAccountsTestState {
            app_state: state,
            redis,
        },
        pool,
        dir,
    )
}

async fn seed_account(pool: &PgPool, account: NewAccount) {
    PgAccountStore::new(pool.clone())
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
