use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{
    PgRuntimeSettingsRepository, RuntimeSettingsRepository, RuntimeSettingsUpdate,
};

use super::TestDatabase;

fn settings_with_margin(refresh_margin_seconds: u64) -> RuntimeSettingsUpdate {
    RuntimeSettingsUpdate {
        admin_api_key: None,
        refresh_margin_seconds,
        refresh_concurrency: 2,
        max_concurrent_per_account: 3,
        request_interval_ms: 50,
        max_waiting_per_key: 0,
        max_waiting_per_account: 0,
        concurrency_wait_timeout_seconds: 30,
        rotation_strategy: "smart".to_owned(),
        model_mappings: BTreeMap::from([
            ("gpt-5.4".to_owned(), "gpt-5.5".to_owned()),
            ("grok-latest".to_owned(), "grok-4.5".to_owned()),
        ]),
        min_codex_desktop_version: None,
        min_codex_cli_version: None,
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
    }
}

#[test]
fn runtime_settings_keep_account_rotation_global() {
    let settings = settings_with_margin(3_600);
    assert!(settings.validate().is_ok());
}

#[test]
fn runtime_settings_reject_invalid_model_mapping() {
    let settings = RuntimeSettingsUpdate {
        model_mappings: BTreeMap::from([("".to_owned(), "gpt-5.5".to_owned())]),
        ..settings_with_margin(3_600)
    };

    assert!(settings.validate().is_err());
}

#[test]
fn runtime_settings_reject_non_semver_client_min() {
    let settings = RuntimeSettingsUpdate {
        min_codex_cli_version: Some("v0.40.0".to_owned()),
        ..settings_with_margin(3_600)
    };

    assert!(settings.validate().is_err());
}

#[tokio::test]
async fn refresh_margin_change_should_preserve_existing_account_refresh_facts() {
    let Some(database) = TestDatabase::create("refresh_margin_reschedule").await else {
        return;
    };
    let expires_at = timestamp_micros(Utc::now() + TimeDelta::hours(2));
    insert_refreshable_account(
        &database.pool,
        "acct_refresh_margin_changed",
        expires_at,
        expires_at - TimeDelta::hours(1),
    )
    .await;
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = account_refresh_facts(&database.pool, "acct_refresh_margin_changed").await;

    repository
        .update_runtime_settings(settings_with_margin(1_800))
        .await
        .expect("update refresh margin");

    assert_eq!(
        account_refresh_facts(&database.pool, "acct_refresh_margin_changed").await,
        before
    );
    database.close().await;
}

#[tokio::test]
async fn client_min_versions_should_round_trip_as_nullable_settings() {
    let Some(database) = TestDatabase::create("client_min_versions").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let mut update = settings_with_margin(3_600);
    update.min_codex_desktop_version = Some("26.825.6671".to_owned());
    update.min_codex_cli_version = Some("0.40.0".to_owned());

    repository
        .update_runtime_settings(update)
        .await
        .expect("update client min versions");
    let settings = repository
        .load_runtime_settings()
        .await
        .expect("load client min versions");

    assert_eq!(
        settings.min_codex_desktop_version.as_deref(),
        Some("26.825.6671")
    );
    assert_eq!(settings.min_codex_cli_version.as_deref(), Some("0.40.0"));
    database.close().await;
}

#[tokio::test]
async fn unchanged_refresh_margin_should_preserve_existing_account_refresh_facts() {
    let Some(database) = TestDatabase::create("refresh_margin_unchanged").await else {
        return;
    };
    let expires_at = timestamp_micros(Utc::now() + TimeDelta::hours(2));
    let retry_at = timestamp_micros(Utc::now() + TimeDelta::minutes(5));
    insert_refreshable_account(
        &database.pool,
        "acct_refresh_margin_unchanged",
        expires_at,
        retry_at,
    )
    .await;
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = account_refresh_facts(&database.pool, "acct_refresh_margin_unchanged").await;

    repository
        .update_runtime_settings(settings_with_margin(3_600))
        .await
        .expect("update unrelated runtime settings");

    assert_eq!(
        account_refresh_facts(&database.pool, "acct_refresh_margin_unchanged").await,
        before
    );
    database.close().await;
}

async fn insert_refreshable_account(
    pool: &sqlx::PgPool,
    account_id: &str,
    expires_at: DateTime<Utc>,
    next_refresh_at: DateTime<Utc>,
) {
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, upstream_user_id, authentication_kind,
           provider_credentials_json, has_refresh_token, access_token_expires_at,
           next_refresh_at, credential_state, credential_observed_at, created_at, updated_at
         ) values ($1, 'openai', $1, $1, 'oauth', '{}'::jsonb, true, $2, $3,
                   'ready', now(), now(), now())",
    )
    .bind(account_id)
    .bind(expires_at)
    .bind(next_refresh_at)
    .execute(pool)
    .await
    .expect("insert refreshable account");
}

#[derive(Debug, PartialEq, Eq)]
struct AccountRefreshFacts {
    access_token_expires_at: DateTime<Utc>,
    next_refresh_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    credential_revision: i64,
}

async fn account_refresh_facts(pool: &sqlx::PgPool, account_id: &str) -> AccountRefreshFacts {
    let (access_token_expires_at, next_refresh_at, updated_at, credential_revision) =
        sqlx::query_as(
            "select access_token_expires_at, next_refresh_at, updated_at, credential_revision
         from provider_accounts
         where id = $1",
        )
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("load account refresh facts");
    AccountRefreshFacts {
        access_token_expires_at,
        next_refresh_at,
        updated_at,
        credential_revision,
    }
}

fn timestamp_micros(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(value.timestamp_micros()).expect("valid test timestamp")
}

#[tokio::test]
async fn concurrency_queue_settings_round_trip_into_the_runtime_snapshot() {
    use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
    let Some(database) = TestDatabase::create("queue_settings").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = repository.load_runtime_settings().await.unwrap();
    assert_eq!(
        (
            before.max_waiting_per_key,
            before.max_waiting_per_account,
            before.concurrency_wait_timeout_seconds
        ),
        (0, 0, 30)
    );
    let mut update = settings_with_margin(3600);
    update.max_waiting_per_key = 5;
    update.max_waiting_per_account = 7;
    update.concurrency_wait_timeout_seconds = 12;
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();
    assert_eq!(
        (
            settings.max_waiting_per_key,
            settings.max_waiting_per_account,
            settings.concurrency_wait_timeout_seconds
        ),
        (5, 7, 12)
    );
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert_eq!(
        (
            snapshot.settings.max_waiting_per_key,
            snapshot.settings.max_waiting_per_account,
            snapshot.settings.concurrency_wait_timeout_seconds
        ),
        (5, 7, 12)
    );
    assert!(snapshot.config_revision > before.config_revision);
    database.close().await;
}
