use codex_proxy_rs::infra::database::connect_sqlite;

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
async fn sqlite_schema_should_persist_token_total_usage_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_usage') where name in ('reasoning_tokens', 'total_tokens') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        ["reasoning_tokens", "total_tokens"]
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
async fn sqlite_schema_should_enforce_unique_chatgpt_identity() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("account-identity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    sqlx::query(
        "insert into accounts (id, chatgpt_account_id, chatgpt_user_id, access_token_cipher, status, added_at, updated_at) values ('acct_a', 'chatgpt-account', 'chatgpt-user', 'cipher', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let result = sqlx::query(
        "insert into accounts (id, chatgpt_account_id, chatgpt_user_id, access_token_cipher, status, added_at, updated_at) values ('acct_b', 'chatgpt-account', 'chatgpt-user', 'cipher', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err());
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
async fn sqlite_schema_should_persist_complete_current_fingerprint_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fingerprints.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('fingerprints') where name in ('originator', 'app_version', 'build_number', 'platform', 'arch', 'chromium_version', 'user_agent_template', 'default_headers_json', 'header_order_json', 'source', 'created_at', 'updated_at') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "app_version",
            "arch",
            "build_number",
            "chromium_version",
            "created_at",
            "default_headers_json",
            "header_order_json",
            "originator",
            "platform",
            "source",
            "updated_at",
            "user_agent_template",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_fingerprint_history_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fingerprint-history.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: Option<(String,)> = sqlx::query_as(
        "select name from sqlite_master where type = 'table' and name = 'fingerprint_update_history'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(row, Some(("fingerprint_update_history".to_string(),)));
}

#[tokio::test]
async fn sqlite_schema_should_persist_structured_event_diagnostic_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("event-diagnostics.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('event_logs') where name in ('transport', 'attempt_index', 'upstream_status_code', 'failure_class', 'response_id', 'upstream_request_id') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "attempt_index",
            "failure_class",
            "response_id",
            "transport",
            "upstream_request_id",
            "upstream_status_code",
        ]
    );
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
