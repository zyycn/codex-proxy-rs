use std::collections::BTreeSet;

use codex_proxy_rs::infra::database::migrate;

use crate::support::storage::init_test_db;

#[tokio::test]
async fn postgres_schema_is_terminal_v1_without_unused_storage() {
    let (pool, _guard) = init_test_db("schema-terminal-v1").await;
    let tables = sqlx::query_scalar::<_, String>(
        "select table_name
         from information_schema.tables
         where table_schema = current_schema() and table_type = 'BASE TABLE'
         order by table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        tables.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from_iter(
            [
                "account_cookies",
                "account_usage",
                "accounts",
                "admin_users",
                "client_api_keys",
                "fingerprint_update_history",
                "fingerprints",
                "ops_error_logs",
                "request_time_buckets",
                "runtime_settings",
                "schema_migrations",
                "usage_records",
            ]
            .map(str::to_string)
        )
    );

    let versions = sqlx::query_as::<_, (i64, String, String)>(
        "select version, name, checksum from schema_migrations order by version",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].0, 1);
    assert_eq!(versions[0].1, "initial");
    assert!(versions.iter().all(|migration| migration.2.len() == 64));
}

#[tokio::test]
async fn postgres_migrate_should_reject_changed_applied_sql_checksum() {
    let (pool, _guard) = init_test_db("schema-checksum-mismatch").await;
    sqlx::query("update schema_migrations set checksum = repeat('0', 64) where version = 1")
        .execute(&pool)
        .await
        .unwrap();

    let error = migrate(&pool).await.unwrap_err();
    assert!(error.to_string().contains("checksum mismatch"));
}

#[tokio::test]
async fn postgres_schema_keeps_retrievable_client_keys_and_hashes_admin_credentials() {
    let (pool, _guard) = init_test_db("schema-credential-storage").await;
    let key_columns = columns(&pool, "client_api_keys").await;
    assert!(key_columns.contains("key"));
    assert!(!key_columns.contains("key_hash"));
    let settings_columns = columns(&pool, "runtime_settings").await;
    assert!(settings_columns.contains("admin_api_key_hash"));
    assert!(!settings_columns.contains("admin_api_key"));

    let key_unique: bool = sqlx::query_scalar(
        "select exists (
           select 1 from pg_indexes
           where schemaname = current_schema()
             and tablename = 'client_api_keys'
             and indexdef ilike '%unique%key%'
         )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(key_unique);
}

#[tokio::test]
async fn postgres_schema_separates_success_and_error_facts() {
    let (pool, _guard) = init_test_db("schema-fact-split").await;
    let usage_columns = columns(&pool, "usage_records").await;
    for column in [
        "client_api_key_id",
        "provider",
        "requested_model",
        "upstream_model",
        "service_tier",
        "first_token_ms",
        "input_tokens",
        "output_tokens",
        "cached_tokens",
        "reasoning_tokens",
    ] {
        assert!(usage_columns.contains(column), "missing {column}");
    }
    assert!(!usage_columns.contains("level"));
    assert!(!usage_columns.contains("failure_class"));

    let error_columns = columns(&pool, "ops_error_logs").await;
    for column in [
        "client_api_key_id",
        "provider",
        "client_status_code",
        "upstream_status_code",
        "failure_class",
    ] {
        assert!(error_columns.contains(column), "missing {column}");
    }

    assert!(sqlx::query(
        "insert into usage_records (
           id, kind, provider, account_id, model, status_code, message, metadata_json, created_at
         ) values ('bad', 'request', 'openai', 'acct', 'gpt-5', 500, 'bad', '{}'::jsonb, now())",
    )
    .execute(&pool)
    .await
    .is_err());
    assert!(sqlx::query(
        "insert into ops_error_logs (id, kind, status_code, message, metadata_json, created_at)
         values ('bad', 'request', 700, 'bad', '{}'::jsonb, now())",
    )
    .execute(&pool)
    .await
    .is_err());
}

#[tokio::test]
async fn postgres_schema_has_required_fact_and_bucket_indexes() {
    let (pool, _guard) = init_test_db("schema-indexes").await;
    let indexes = sqlx::query_scalar::<_, String>(
        "select indexname from pg_indexes where schemaname = current_schema()",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect::<BTreeSet<_>>();
    for index in [
        "idx_usage_records_created_id",
        "idx_usage_records_key_created",
        "idx_usage_records_response_id",
        "idx_ops_error_logs_created_id",
        "idx_ops_error_logs_key_created",
        "idx_ops_error_logs_failure_class",
    ] {
        assert!(indexes.contains(index), "missing {index}");
    }
    for retired_index in [
        "idx_accounts_status",
        "idx_account_usage_last_used_account",
        "idx_request_time_buckets_model",
    ] {
        assert!(!indexes.contains(retired_index), "retained {retired_index}");
    }
}

#[tokio::test]
async fn postgres_schema_enforces_account_and_counter_constraints() {
    let (pool, _guard) = init_test_db("schema-constraints").await;
    assert!(sqlx::query(
        "insert into accounts (id, access_token, status, added_at, updated_at)
         values ('acct_bad', 'token', 'refreshing', now(), now())",
    )
    .execute(&pool)
    .await
    .is_err());
    assert!(sqlx::query(
        "insert into request_time_buckets (
           bucket_start, provider, account_id, model, service_tier, success_count
         ) values (now(), 'openai', 'acct', 'gpt-5', '__unknown__', -1)",
    )
    .execute(&pool)
    .await
    .is_err());
    sqlx::query(
        "insert into runtime_settings (
           id, refresh_margin_seconds, refresh_concurrency,
           max_concurrent_per_account, request_interval_ms,
           rotation_strategy, updated_at
         ) values (1, 3600, 2, 1, 0, 'smart', now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::query("update runtime_settings set usage_retention_days = 0 where id = 1",)
            .execute(&pool)
            .await
            .is_err()
    );
}

async fn columns(pool: &sqlx::PgPool, table: &str) -> BTreeSet<String> {
    sqlx::query_scalar::<_, String>(
        "select column_name
         from information_schema.columns
         where table_schema = current_schema() and table_name = $1",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .collect()
}
