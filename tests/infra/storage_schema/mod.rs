use codex_proxy_rs::infra::database::connect_sqlite;

#[tokio::test]
async fn sqlite_schema_creates_current_runtime_tables_without_migration_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: (i64,) = sqlx::query_as(
        "select count(*) from sqlite_master where type = 'table' and name in ('accounts', 'client_api_keys', 'runtime_settings', 'event_logs', 'model_plan_snapshots', 'account_model_usage', 'usage_time_buckets', 'model_account_routes')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, 8);

    let migration_table: Option<(String,)> = sqlx::query_as(
        "select name from sqlite_master where type = 'table' and name = 'schema_migrations'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(migration_table, None);
}

#[tokio::test]
async fn sqlite_schema_should_persist_runtime_settings_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("settings.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('runtime_settings') where name in ('id', 'default_model', 'model_aliases_json', 'refresh_margin_seconds', 'refresh_concurrency', 'max_concurrent_per_account', 'request_interval_ms', 'rotation_strategy', 'updated_at') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "default_model",
            "id",
            "max_concurrent_per_account",
            "model_aliases_json",
            "refresh_concurrency",
            "refresh_margin_seconds",
            "request_interval_ms",
            "rotation_strategy",
            "updated_at",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_runtime_query_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("indexes.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from sqlite_master where type = 'index' and name in ('idx_accounts_added_id', 'idx_account_usage_last_used_account', 'idx_account_model_usage_last_used', 'idx_client_api_keys_created_id', 'idx_fingerprint_update_history_created_id') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "idx_account_model_usage_last_used",
            "idx_account_usage_last_used_account",
            "idx_accounts_added_id",
            "idx_client_api_keys_created_id",
            "idx_fingerprint_update_history_created_id",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_diagnostic_filter_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("diagnostic-indexes.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from sqlite_master where type = 'index' and name in ('idx_account_cookies_expires', 'idx_event_logs_level_created', 'idx_event_logs_model_created', 'idx_event_logs_route_created', 'idx_event_logs_status_created', 'idx_event_logs_upstream_status_created', 'idx_session_affinities_active_order', 'idx_model_account_routes_account', 'idx_model_account_routes_enabled_model', 'idx_usage_time_buckets_bucket', 'idx_usage_time_buckets_model_bucket') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "idx_account_cookies_expires",
            "idx_event_logs_level_created",
            "idx_event_logs_model_created",
            "idx_event_logs_route_created",
            "idx_event_logs_status_created",
            "idx_event_logs_upstream_status_created",
            "idx_model_account_routes_account",
            "idx_model_account_routes_enabled_model",
            "idx_session_affinities_active_order",
            "idx_usage_time_buckets_bucket",
            "idx_usage_time_buckets_model_bucket",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_account_model_usage_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model-usage.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_model_usage') where name in ('account_id', 'model', 'request_count', 'error_count', 'input_tokens', 'output_tokens', 'cached_tokens', 'last_used_at') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "account_id",
            "cached_tokens",
            "error_count",
            "input_tokens",
            "last_used_at",
            "model",
            "output_tokens",
            "request_count",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_usage_time_bucket_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("usage-time-buckets.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('usage_time_buckets') where name in ('bucket_start', 'account_id', 'model', 'service_tier', 'request_count', 'error_count', 'input_tokens', 'output_tokens', 'cached_tokens', 'first_token_latency_sum', 'first_token_latency_count', 'latency_sum', 'latency_count', 'max_latency_ms', 'min_latency_ms', 'updated_at') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "account_id",
            "bucket_start",
            "cached_tokens",
            "error_count",
            "first_token_latency_count",
            "first_token_latency_sum",
            "input_tokens",
            "latency_count",
            "latency_sum",
            "max_latency_ms",
            "min_latency_ms",
            "model",
            "output_tokens",
            "request_count",
            "service_tier",
            "updated_at",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_model_account_routes_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model-account-routes.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let rows: Vec<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('model_account_routes') where name in ('model', 'account_id', 'priority', 'enabled', 'created_at', 'updated_at') order by name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        [
            "account_id",
            "created_at",
            "enabled",
            "model",
            "priority",
            "updated_at",
        ]
    );
}

#[tokio::test]
async fn sqlite_schema_should_persist_plain_account_cookie_value_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let value_column: Option<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_cookies') where name = 'value'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    let cipher_column: Option<(String,)> = sqlx::query_as(
        "select name from pragma_table_info('account_cookies') where name = 'value_cipher'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(value_column, Some(("value".to_string(),)));
    assert_eq!(cipher_column, None);
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
        "insert into accounts (id, chatgpt_account_id, chatgpt_user_id, access_token, status, added_at, updated_at) values ('acct_a', 'chatgpt-account', 'chatgpt-user', 'access-token-a', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let result = sqlx::query(
        "insert into accounts (id, chatgpt_account_id, chatgpt_user_id, access_token, status, added_at, updated_at) values ('acct_b', 'chatgpt-account', 'chatgpt-user', 'access-token-b', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
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
        "insert into accounts (id, access_token, status, added_at, updated_at) values ('acct_bad', 'access-token', 'unknown', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
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
        "insert into client_api_keys (id, name, prefix, key_hash, key, enabled, created_at) values ('key_bad', 'bad', 'sk_x', 'hash', 'sk_test', 2, '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    let account_result = sqlx::query(
        "insert into accounts (id, access_token, status, quota_limit_reached, added_at, updated_at) values ('acct_bad', 'access-token', 'active', 2, '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    let quota_verify_result = sqlx::query(
        "insert into accounts (id, access_token, status, quota_verify_required, added_at, updated_at) values ('acct_verify_bad', 'access-token', 'active', 2, '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values ('acct_route', 'access-token', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let model_route_result = sqlx::query(
        "insert into model_account_routes (model, account_id, enabled, created_at, updated_at) values ('gpt-5.5', 'acct_route', 2, '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(api_key_result.is_err());
    assert!(account_result.is_err());
    assert!(quota_verify_result.is_err());
    assert!(model_route_result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_invalid_runtime_settings() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("settings-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let invalid_id_result = sqlx::query(
        "insert into runtime_settings (id, default_model, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at) values (2, 'gpt-5.5', 300, 2, 3, 50, 'least_used', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    let invalid_refresh_result = sqlx::query(
        "insert into runtime_settings (id, default_model, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at) values (1, 'gpt-5.5', 0, 2, 3, 50, 'least_used', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;
    let invalid_strategy_result = sqlx::query(
        "insert into runtime_settings (id, default_model, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at) values (1, 'gpt-5.5', 300, 2, 3, 50, 'random', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(invalid_id_result.is_err());
    assert!(invalid_refresh_result.is_err());
    assert!(invalid_strategy_result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_invalid_model_account_route_priority() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model-route-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values ('acct_route', 'access-token', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let result = sqlx::query(
        "insert into model_account_routes (model, account_id, priority, created_at, updated_at) values ('gpt-5.5', 'acct_route', -1, '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_negative_usage_time_bucket_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("usage-time-bucket-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let result = sqlx::query(
        "insert into usage_time_buckets (bucket_start, request_count, updated_at) values ('2026-06-14T00:00:00Z', -1, '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn sqlite_schema_should_reject_negative_account_usage_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("usage-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values ('acct_a', 'access-token', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
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
async fn sqlite_schema_should_reject_negative_account_model_usage_counts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("model-usage-check.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at) values ('acct_a', 'access-token', 'active', '2026-06-14T00:00:00Z', '2026-06-14T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let result = sqlx::query(
        "insert into account_model_usage (account_id, model, request_count) values ('acct_a', 'gpt-5.5', -1)",
    )
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
