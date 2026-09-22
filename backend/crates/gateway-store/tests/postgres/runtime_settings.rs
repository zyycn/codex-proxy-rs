use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{
    PgRuntimeSettingsRepository, RuntimeSettingsRepository, RuntimeSettingsUpdate,
};

use super::TestDatabase;

fn settings_with_margin(refresh_margin_seconds: u64) -> RuntimeSettingsUpdate {
    RuntimeSettingsUpdate {
        openai_client_profile: None,
        xai_client_profile: None,
        request_location_enabled: false,
        request_location: Default::default(),
        admin_api_key: None,
        refresh_margin_seconds,
        refresh_concurrency: 2,
        max_concurrent_per_account: 3,
        request_interval_ms: 50,
        max_waiting_per_key: 0,
        max_waiting_per_account: 0,
        concurrency_wait_timeout_seconds: 30,
        responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
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
        account_auto_freeze_enabled: true,
        account_auto_freeze_threshold: 12,
        account_auto_freeze_window_seconds: 600,
        account_auto_freeze_duration_seconds: 7_200,
        account_auto_freeze_probe_enabled: true,
        account_auto_freeze_probe_model: None,
        account_auto_freeze_adaptive_concurrency: true,
    }
}

#[test]
fn runtime_settings_keep_account_rotation_global() {
    let settings = settings_with_margin(3_600);
    assert!(settings.validate().is_ok());
}

#[tokio::test]
async fn unlimited_default_account_concurrency_round_trips_without_relaxing_other_limits() {
    let Some(database) = TestDatabase::create("unlimited_default_concurrency").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let mut update = settings_with_margin(3_600);
    update.max_concurrent_per_account = 0;
    repository
        .update_runtime_settings(update)
        .await
        .expect("persist unlimited default");
    let settings = repository
        .load_runtime_settings()
        .await
        .expect("read unlimited default");
    assert_eq!(settings.max_concurrent_per_account, 0);
    for statement in [
        "update runtime_settings set max_concurrent_per_account = -1 where id = 1",
        "update runtime_settings set refresh_concurrency = 0 where id = 1",
        "update runtime_settings set refresh_margin_seconds = 0 where id = 1",
        "update runtime_settings set request_interval_ms = -1 where id = 1",
    ] {
        let error = sqlx::query(statement)
            .execute(&database.pool)
            .await
            .expect_err("constraint must reject invalid setting");
        assert_eq!(
            error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("23514"),
            "{statement}"
        );
    }
    database.close().await;
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
fn runtime_settings_reject_out_of_range_auto_freeze() {
    for update in [
        RuntimeSettingsUpdate {
            account_auto_freeze_threshold: 1,
            ..settings_with_margin(3_600)
        },
        RuntimeSettingsUpdate {
            account_auto_freeze_window_seconds: 59,
            ..settings_with_margin(3_600)
        },
        RuntimeSettingsUpdate {
            account_auto_freeze_duration_seconds: 299,
            ..settings_with_margin(3_600)
        },
        RuntimeSettingsUpdate {
            account_auto_freeze_probe_model: Some(" pad ".to_owned()),
            ..settings_with_margin(3_600)
        },
    ] {
        assert!(update.validate().is_err());
    }
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

#[tokio::test]
async fn request_location_defaults_and_updates_reach_the_runtime_snapshot() {
    use gateway_core::account::RequestLocation;
    use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
    let Some(database) = TestDatabase::create("global_request_location").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = repository.load_runtime_settings().await.unwrap();
    assert_eq!(before.request_location, RequestLocation::default());
    assert!(!before.request_location_enabled);
    let mut update = settings_with_margin(3600);
    update.request_location = serde_json::from_value(serde_json::json!({"country":"JP", "region":" Tokyo ", "city":" Tokyo ", "timezone":"Asia/Tokyo"})).unwrap();
    update.request_location_enabled = true;
    update.account_auto_freeze_threshold = 17;
    update.account_auto_freeze_window_seconds = 900;
    update.account_auto_freeze_duration_seconds = 3_600;
    update.account_auto_freeze_probe_model = Some("gpt-5.5".to_owned());
    update.account_auto_freeze_adaptive_concurrency = false;
    let expected = update.request_location.clone().normalized().unwrap();
    let mut disabled = update.clone();
    disabled.request_location = expected.clone();
    disabled.request_location_enabled = false;
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert_eq!(settings.request_location, expected);
    assert_eq!(snapshot.settings.request_location, expected);
    assert!(settings.request_location_enabled);
    assert!(snapshot.settings.request_location_enabled);
    assert!(snapshot.config_revision > before.config_revision);
    repository.update_runtime_settings(disabled).await.unwrap();
    let disabled_settings = repository.load_runtime_settings().await.unwrap();
    assert!(!disabled_settings.request_location_enabled);
    assert_eq!(disabled_settings.request_location, expected);
    let disabled_snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert!(!disabled_snapshot.settings.request_location_enabled);
    assert_eq!(disabled_snapshot.settings.request_location, expected);
    // 位置开关与自动冻结共用设置写入，切换位置不能覆盖冻结参数。
    for saved in [&settings, &disabled_settings] {
        assert!(saved.account_auto_freeze_enabled);
        assert_eq!(saved.account_auto_freeze_threshold, 17);
        assert_eq!(saved.account_auto_freeze_window_seconds, 900);
        assert_eq!(saved.account_auto_freeze_duration_seconds, 3_600);
        assert!(saved.account_auto_freeze_probe_enabled);
        assert_eq!(
            saved.account_auto_freeze_probe_model.as_deref(),
            Some("gpt-5.5")
        );
        assert!(!saved.account_auto_freeze_adaptive_concurrency);
    }
    assert!(disabled_snapshot.config_revision > snapshot.config_revision);
    for invalid in [
        serde_json::json!(null),
        serde_json::json!({}),
        serde_json::json!({"country":"US", "region":"Ohio", "city":null, "timezone":"America/New_York"}),
    ] {
        assert!(
            sqlx::query("update runtime_settings set request_location_json = $1 where id = 1")
                .bind(sqlx::types::Json(invalid))
                .execute(&database.pool)
                .await
                .is_err()
        );
    }
    assert_eq!(
        repository
            .load_runtime_settings()
            .await
            .unwrap()
            .request_location,
        expected
    );
    database.close().await;
}

#[tokio::test]
async fn auto_freeze_defaults_off_and_explicit_opt_in_round_trips() {
    let Some(database) = TestDatabase::create("freeze_opt_in").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    assert!(
        !repository
            .load_runtime_settings()
            .await
            .expect("default settings")
            .account_auto_freeze_enabled
    );
    repository
        .update_runtime_settings(settings_with_margin(3_600))
        .await
        .expect("explicit opt-in");
    assert!(
        repository
            .load_runtime_settings()
            .await
            .expect("settings")
            .account_auto_freeze_enabled
    );
    database.close().await;
}

#[tokio::test]
async fn decompression_setting_should_persist_and_reach_snapshot_facts() {
    use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
    let Some(database) = TestDatabase::create("decompression_settings").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = repository.load_runtime_settings().await.unwrap();
    assert_eq!(
        before.responses_max_decompressed_body_bytes,
        64 * 1024 * 1024
    );
    let mut update = settings_with_margin(3600);
    update.responses_max_decompressed_body_bytes = 128 * 1024 * 1024;
    repository.update_runtime_settings(update).await.unwrap();
    let reloaded = PgRuntimeSettingsRepository::new(database.pool.clone())
        .load_runtime_settings()
        .await
        .unwrap();
    assert_eq!(
        reloaded.responses_max_decompressed_body_bytes,
        128 * 1024 * 1024
    );
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert_eq!(
        snapshot.settings.responses_max_decompressed_body_bytes,
        reloaded.responses_max_decompressed_body_bytes
    );
    assert!(snapshot.config_revision > before.config_revision);
    for invalid in [0, u64::MAX] {
        let mut update = settings_with_margin(3600);
        update.responses_max_decompressed_body_bytes = invalid;
        assert!(repository.update_runtime_settings(update).await.is_err());
        assert_eq!(
            repository
                .load_runtime_settings()
                .await
                .unwrap()
                .config_revision,
            reloaded.config_revision
        );
    }
    database.close().await;
}

#[tokio::test]
async fn request_profile_initialization_is_idempotent_and_old_updates_preserve_it() {
    use gateway_core::{
        account::OpaqueProviderData, provider_ports::ProviderRuntimePolicyPort,
        routing::ProviderKind,
    };
    let Some(database) = TestDatabase::create("request_profiles").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let provider = ProviderKind::new("openai").unwrap();
    let document = |name| {
        OpaqueProviderData::new(
            serde_json::json!({"marker":name})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    let initial = document("imported");
    assert_eq!(
        repository
            .initialize_request_profile(&provider, initial.clone())
            .await
            .unwrap(),
        initial
    );
    let revision = repository
        .load_runtime_settings()
        .await
        .unwrap()
        .config_revision;
    assert_eq!(
        repository
            .initialize_request_profile(&provider, document("ignored"))
            .await
            .unwrap(),
        initial
    );
    assert_eq!(
        repository
            .load_runtime_settings()
            .await
            .unwrap()
            .config_revision,
        revision
    );
    repository
        .update_runtime_settings(settings_with_margin(3600))
        .await
        .unwrap();
    assert_eq!(
        repository
            .load_runtime_settings()
            .await
            .unwrap()
            .openai_client_profile,
        Some(initial)
    );
    let mut update = settings_with_margin(3600);
    update.openai_client_profile = Some(document("edited"));
    repository.update_runtime_settings(update).await.unwrap();
    assert_eq!(
        repository
            .initialize_request_profile(&provider, document("old-yaml"))
            .await
            .unwrap(),
        document("edited")
    );
    database.close().await;
}

#[tokio::test]
async fn xai_profile_initialization_and_updates_preserve_other_providers() {
    use gateway_core::{
        account::OpaqueProviderData, provider_ports::ProviderRuntimePolicyPort,
        routing::ProviderKind,
    };
    let Some(database) = TestDatabase::create("xai_profiles").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let document = |version: &str| {
        OpaqueProviderData::new(
            serde_json::json!({"clientVersion":version})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    let provider = ProviderKind::new("xai").unwrap();
    repository
        .initialize_request_profile(&provider, document("initial"))
        .await
        .unwrap();
    assert_eq!(
        repository
            .initialize_request_profile(&provider, document("ignored"))
            .await
            .unwrap(),
        document("initial")
    );
    let mut update = settings_with_margin(3600);
    update.openai_client_profile = Some(document("openai"));
    update.xai_client_profile = Some(document("xai"));
    repository.update_runtime_settings(update).await.unwrap();
    let mut update = settings_with_margin(3600);
    update.xai_client_profile = Some(document("edited"));
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();
    assert_eq!(settings.openai_client_profile, Some(document("openai")));
    assert_eq!(settings.xai_client_profile, Some(document("edited")));
    assert_eq!(
        repository
            .initialize_request_profile(&provider, document("old-yaml"))
            .await
            .unwrap(),
        document("edited")
    );
    database.close().await;
}
