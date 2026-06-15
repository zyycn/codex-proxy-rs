use std::str::FromStr;

use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

use codex_proxy_rs::platform::storage::db::connect_sqlite;

#[tokio::test]
async fn sqlite_schema_creates_accounts_and_event_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: (i64,) = sqlx::query_as(
        "select count(*) from sqlite_master where type = 'table' and name in ('accounts', 'client_api_keys', 'event_logs', 'model_plan_snapshots')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, 4);
}

#[tokio::test]
async fn sqlite_schema_should_persist_account_usage_window_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_usage') where name in ('window_request_count', 'window_input_tokens', 'window_output_tokens', 'window_cached_tokens', 'window_started_at', 'window_reset_at', 'limit_window_seconds') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "limit_window_seconds",
            "window_cached_tokens",
            "window_input_tokens",
            "window_output_tokens",
            "window_request_count",
            "window_reset_at",
            "window_started_at",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_image_generation_usage_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_usage') where name in ('image_input_tokens', 'image_output_tokens', 'image_request_count', 'image_request_failed_count', 'window_image_input_tokens', 'window_image_output_tokens', 'window_image_request_count', 'window_image_request_failed_count') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "image_input_tokens",
            "image_output_tokens",
            "image_request_count",
            "image_request_failed_count",
            "window_image_input_tokens",
            "window_image_output_tokens",
            "window_image_request_count",
            "window_image_request_failed_count",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_quota_verify_required_flag() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: Option<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('accounts') where name = 'quota_verify_required'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(row, Some(("quota_verify_required".to_string(),)));
}

#[tokio::test]
async fn sqlite_schema_should_persist_session_affinity_function_call_ids() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: Option<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('session_affinities') where name = 'function_call_ids_json'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(row, Some(("function_call_ids_json".to_string(),)));
}

#[tokio::test]
async fn connect_sqlite_should_not_patch_incomplete_existing_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("incomplete.sqlite");
    let url = format!("sqlite://{}", db.display());
    let incomplete_options = SqliteConnectOptions::from_str(&url)
        .unwrap()
        .create_if_missing(true);
    let incomplete_pool = SqlitePool::connect_with(incomplete_options).await.unwrap();
    sqlx::raw_sql(
        "
        create table account_usage (
          account_id text primary key,
          request_count integer not null default 0,
          input_tokens integer not null default 0,
          output_tokens integer not null default 0,
          cached_tokens integer not null default 0,
          last_used_at text
        );
        create table session_affinities (
          response_id text primary key,
          account_id text not null,
          conversation_id text not null,
          turn_state text,
          instructions_hash text,
          input_tokens integer,
          variant_hash text,
          expires_at text not null,
          created_at text not null
        );
        ",
    )
    .execute(&incomplete_pool)
    .await
    .unwrap();
    incomplete_pool.close().await;

    let pool = connect_sqlite(&url).await.unwrap();
    let window_column: Option<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_usage') where name = 'window_request_count'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let affinity_column: Option<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('session_affinities') where name = 'function_call_ids_json'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(window_column, None);
    assert_eq!(affinity_column, None);
}

#[tokio::test]
async fn sqlite_schema_should_reject_invalid_account_status() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("status-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let result = sqlx::query(
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values ('acct_bad', 'cipher', 'unknown', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_invalid_event_log_level() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("event-level-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let result = sqlx::query(
        "insert into event_logs (id, kind, level, message, metadata_json, created_at) values ('log_bad', 'request', 'fatal', 'bad', '{}', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_non_boolean_flags() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("boolean-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let api_key_result = sqlx::query(
        "insert into client_api_keys (id, name, prefix, key_hash, enabled, created_at) values ('key_bad', 'bad', 'cpr_x', 'hash', 2, '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    let account_result = sqlx::query(
        "insert into accounts (id, access_token_cipher, status, quota_limit_reached, added_at, updated_at) values ('acct_bad', 'cipher', 'active', 2, '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    let quota_verify_result = sqlx::query(
        "insert into accounts (id, access_token_cipher, status, quota_verify_required, added_at, updated_at) values ('acct_verify_bad', 'cipher', 'active', 2, '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(api_key_result.is_err());
    assert!(account_result.is_err());
    assert!(quota_verify_result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_negative_account_usage_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("usage-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    sqlx::query(
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values ('acct_a', 'cipher', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let result =
        sqlx::query("insert into account_usage (account_id, request_count) values ('acct_a', -1)")
            .execute(&pool)
            .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn connect_sqlite_creates_parent_directory_for_file_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nested").join("startup.sqlite");
    let url = format!("sqlite://{}", db.display());

    let pool = connect_sqlite(&url).await.unwrap();
    pool.close().await;

    assert!(db.exists());
}
