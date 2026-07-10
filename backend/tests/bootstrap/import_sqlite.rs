use std::path::PathBuf;

use codex_proxy_rs::bootstrap::import_sqlite::{import_sqlite, ImportSqliteError};
use sqlx::{sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions, Row};

use crate::support::storage::init_test_db;

#[tokio::test]
async fn import_sqlite_should_split_facts_preserve_retrievable_keys_and_report_discards() {
    let (_source_guard, source_path) = sqlite_v3_source("success").await;
    seed_representative_v3_data(&source_path).await;
    let (target, _target_guard) = init_test_db("import-sqlite-success").await;

    let report = import_sqlite(&target, &source_path).await.unwrap();

    assert_eq!(report.imported_rows("client_api_keys"), 1);
    assert_eq!(report.imported_rows("accounts"), 2);
    assert_eq!(report.normalized_rows("accounts.refreshing_to_expired"), 1);
    assert_eq!(report.imported_rows("usage_records"), 1);
    assert_eq!(report.imported_rows("ops_error_logs"), 2);
    assert_eq!(report.discarded_rows("usage_record_noise"), 1);
    assert_eq!(report.discarded_rows("admin_sessions"), 1);
    assert_eq!(report.discarded_rows("session_affinities"), 1);
    assert_eq!(report.discarded_rows("account_refresh_leases"), 1);
    assert_eq!(report.discarded_rows("model_plan_snapshots"), 1);
    assert_eq!(report.cookie_expiration_parse_failures(), 1);

    let key: String = sqlx::query_scalar("select key from client_api_keys where id = $1")
        .bind("key-1")
        .fetch_one(&target)
        .await
        .unwrap();
    assert_eq!(key, "sk-old-secret");

    let refreshing_account: (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("select status, next_refresh_at from accounts where id = $1")
            .bind("acct-refreshing")
            .fetch_one(&target)
            .await
            .unwrap();
    assert_eq!(refreshing_account.0, "expired");
    assert!(refreshing_account.1.is_none());

    let success = sqlx::query(
        "select requested_model, upstream_model, service_tier, first_token_ms,
                input_tokens, output_tokens, cached_tokens, reasoning_tokens, metadata_json
         from usage_records where id = $1",
    )
    .bind("usage-success")
    .fetch_one(&target)
    .await
    .unwrap();
    assert_eq!(
        success
            .get::<Option<String>, _>("requested_model")
            .as_deref(),
        Some("alias")
    );
    assert_eq!(
        success
            .get::<Option<String>, _>("upstream_model")
            .as_deref(),
        Some("gpt-5.5")
    );
    assert_eq!(
        success.get::<Option<String>, _>("service_tier").as_deref(),
        Some("priority")
    );
    assert_eq!(success.get::<Option<i64>, _>("first_token_ms"), Some(0));
    assert_eq!(success.get::<Option<i64>, _>("input_tokens"), Some(12));
    assert_eq!(success.get::<Option<i64>, _>("output_tokens"), Some(5));
    assert_eq!(success.get::<Option<i64>, _>("cached_tokens"), Some(3));
    assert_eq!(success.get::<Option<i64>, _>("reasoning_tokens"), Some(2));
    let metadata: serde_json::Value = success.get("metadata_json");
    assert_eq!(metadata["clientIp"], "127.0.0.1");
    assert!(metadata.get("usage").is_none());

    let bucket: (String, String, String, i64, i64, Option<i64>) = sqlx::query_as(
        "select provider, account_id, model, success_count, error_count, min_latency_ms
         from request_time_buckets",
    )
    .fetch_one(&target)
    .await
    .unwrap();
    assert_eq!(bucket.0, "openai");
    assert_eq!(bucket.1, "__unknown__");
    assert_eq!(bucket.2, "__unknown__");
    assert_eq!(bucket.3, 2);
    assert_eq!(bucket.4, 1);
    assert_eq!(bucket.5, None);
}

#[tokio::test]
async fn import_sqlite_should_rollback_all_target_writes_on_invalid_source_value() {
    let (_source_guard, source_path) = sqlite_v3_source("rollback").await;
    let source = open_sqlite(&source_path, false).await;
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at)
         values ('admin', 'hash', 'not-a-time', '2026-01-01T00:00:00Z')",
    )
    .execute(&source)
    .await
    .unwrap();
    source.close().await;
    let (target, _target_guard) = init_test_db("import-sqlite-rollback").await;

    let error = import_sqlite(&target, &source_path).await.unwrap_err();

    assert!(matches!(error, ImportSqliteError::InvalidTimestamp { .. }));
    let rows: i64 = sqlx::query_scalar("select count(*) from admin_users")
        .fetch_one(&target)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn import_sqlite_should_reject_unknown_source_version() {
    let (_source_guard, source_path) = sqlite_v3_source("version").await;
    let source = open_sqlite(&source_path, false).await;
    sqlx::query("delete from schema_migrations where version = 3")
        .execute(&source)
        .await
        .unwrap();
    source.close().await;
    let (target, _target_guard) = init_test_db("import-sqlite-version").await;

    let error = import_sqlite(&target, &source_path).await.unwrap_err();

    assert!(matches!(
        error,
        ImportSqliteError::UnsupportedSourceVersion { actual: Some(2) }
    ));
}

#[tokio::test]
async fn import_sqlite_should_reject_nonempty_target() {
    let (_source_guard, source_path) = sqlite_v3_source("nonempty").await;
    let (target, _target_guard) = init_test_db("import-sqlite-nonempty").await;
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at)
         values ($1, $2, now(), now())",
    )
    .bind("existing")
    .bind("hash")
    .execute(&target)
    .await
    .unwrap();

    let error = import_sqlite(&target, &source_path).await.unwrap_err();

    assert!(matches!(error, ImportSqliteError::TargetNotEmpty));
}

async fn sqlite_v3_source(label: &str) -> (tempfile::TempDir, PathBuf) {
    let guard = tempfile::tempdir().unwrap();
    let path = guard.path().join(format!("{label}.sqlite"));
    let source = open_sqlite(&path, true).await;
    sqlx::raw_sql(include_str!("../fixtures/sqlite_v3.sql"))
        .execute(&source)
        .await
        .unwrap();
    source.close().await;
    (guard, path)
}

async fn open_sqlite(path: &PathBuf, create: bool) -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(create),
        )
        .await
        .unwrap()
}

async fn seed_representative_v3_data(path: &PathBuf) {
    let source = open_sqlite(path, false).await;
    sqlx::raw_sql(
        r#"
insert into admin_users values
  ('admin', 'argon-hash', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
insert into admin_sessions values
  ('session', 'admin', '2026-01-02T00:00:00Z', '2026-01-01T00:00:00Z');
insert into client_api_keys values
  ('key-1', 'legacy', 'sk-old', 'sk-old-secret', null, 1,
   '2026-01-01T00:00:00Z', null);
insert into runtime_settings values
  (1, '{}', 3600, 2, 3, 50, 'smart', 'admin-machine-secret',
   '2026-01-01T00:00:00Z');
insert into accounts
  (id, email, chatgpt_account_id, chatgpt_user_id, label, plan_type,
   access_token, refresh_token, access_token_expires_at, next_refresh_at,
   status, quota_json, quota_fetched_at, quota_limit_reached,
   quota_verify_required, quota_cooldown_until, cloudflare_cooldown_until,
   added_at, updated_at)
values
  ('acct-1', 'a@example.com', 'chatgpt-1', 'user-1', null, 'plus',
   'access', 'refresh', '2026-02-01T00:00:00Z', '2026-01-31T23:00:00Z',
   'active', '{"remaining":1}', '2026-01-01T00:00:00Z', 0, 0, null, null,
   '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
  ('acct-refreshing', 'refreshing@example.com', 'chatgpt-refreshing', 'user-refreshing',
   null, 'plus', 'access-refreshing', 'refresh-refreshing', '2026-02-01T00:00:00Z',
   '2026-01-31T23:00:00Z', 'refreshing', null, null, 0, 0, null, null,
   '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
insert into account_cookies values
  ('cookie-1', 'acct-1', '.chatgpt.com', 'session', 'secret', '/',
   'not-a-cookie-date', '2026-01-01T00:00:00Z');
insert into account_refresh_leases values
  ('acct-1', 'owner', '2026-01-01T00:05:00Z', '2026-01-01T00:00:00Z');
insert into model_plan_snapshots values
  ('plus', '[]', '2026-01-01T00:00:00Z');
insert into session_affinities values
  ('response-1', 'acct-1', 'conversation-1', null, null, null, '[]', null,
   '2026-01-02T00:00:00Z', '2026-01-01T00:00:00Z');
insert into usage_records
  (id, request_id, kind, level, account_id, route, model, status_code,
   transport, attempt_index, upstream_status_code, failure_class, response_id,
   upstream_request_id, latency_ms, message, metadata_json, created_at)
values
  ('usage-success', 'req-1', 'v1.response', 'info', 'acct-1', '/v1/responses',
   'gpt-5.5', 200, 'http', 0, 200, null, 'resp-1', 'upstream-1', 0,
   'ok',
   '{"requestedModel":"alias","upstreamModel":"gpt-5.5","serviceTier":"priority","firstTokenMs":0,"usage":{"inputTokens":12,"outputTokens":5,"cachedTokens":3,"reasoningTokens":2},"clientIp":"127.0.0.1"}',
   '2026-01-01T00:01:00Z'),
  ('usage-error', 'req-2', 'v1.response', 'error', 'acct-1', '/v1/responses',
   'gpt-5.5', 502, 'http', 1, 500, 'upstream', null, null, 10,
   'failed', '{}', '2026-01-01T00:02:00Z'),
  ('usage-noise', null, 'diagnostic', 'info', null, null, null, null, null,
   null, null, null, null, null, null, 'noise', '{}', '2026-01-01T00:03:00Z');
insert into ops_error_logs
  (id, request_id, kind, account_id, route, model, status_code,
   client_status_code, upstream_status_code, transport, attempt_index,
   failure_class, response_id, upstream_request_id, latency_ms, message,
   metadata_json, created_at)
values
  ('ops-1', 'req-3', 'v1.response', 'acct-1', '/v1/responses', 'gpt-5.5',
   429, 429, 429, 'http', 0, 'quota', null, null, 5, 'limited', '{}',
   '2026-01-01T00:04:00Z');
insert into usage_time_buckets
  (bucket_start, account_id, model, service_tier, request_count, error_count,
   input_tokens, output_tokens, cached_tokens, first_token_latency_sum,
   first_token_latency_count, latency_sum, latency_count, max_latency_ms,
   min_latency_ms, updated_at)
values
  ('2026-01-01T00:00:00Z', '', '', '', 3, 1, 12, 5, 3, 0, 1, 0, 1, 0, 0,
   '2026-01-01T00:15:00Z');
"#,
    )
    .execute(&source)
    .await
    .unwrap();
    source.close().await;
}
