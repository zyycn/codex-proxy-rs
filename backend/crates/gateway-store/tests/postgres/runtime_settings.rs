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
        rotation_strategy: "smart".to_owned(),
        model_mappings: BTreeMap::from([
            ("gpt-5.4".to_owned(), "gpt-5.5".to_owned()),
            ("grok-latest".to_owned(), "grok-4.5".to_owned()),
        ]),
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

#[tokio::test]
async fn refresh_margin_change_should_reschedule_existing_refreshable_accounts() {
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

    repository
        .update_runtime_settings(settings_with_margin(1_800))
        .await
        .expect("update refresh margin");

    assert_eq!(
        account_next_refresh_at(&database.pool, "acct_refresh_margin_changed").await,
        expires_at - TimeDelta::seconds(1_800)
    );
    database.close().await;
}

#[tokio::test]
async fn unchanged_refresh_margin_should_preserve_existing_retry_schedule() {
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

    repository
        .update_runtime_settings(settings_with_margin(3_600))
        .await
        .expect("update unrelated runtime settings");

    assert_eq!(
        account_next_refresh_at(&database.pool, "acct_refresh_margin_unchanged").await,
        retry_at
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
           next_refresh_at, availability, availability_observed_at, created_at, updated_at
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

async fn account_next_refresh_at(pool: &sqlx::PgPool, account_id: &str) -> DateTime<Utc> {
    sqlx::query_scalar("select next_refresh_at from provider_accounts where id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("load next refresh time")
}

fn timestamp_micros(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(value.timestamp_micros()).expect("valid test timestamp")
}
