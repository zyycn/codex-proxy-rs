//! 验证运行设置的数据库约束、升级保留与快照发布

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{
    PgRuntimeSettingsRepository, RuntimeSettingsRepository, RuntimeSettingsUpdate,
};

use super::TestDatabase;

pub(super) fn settings_with_margin(refresh_margin_seconds: u64) -> RuntimeSettingsUpdate {
    RuntimeSettingsUpdate {
        request_profile_updates: BTreeMap::new(),
        rotation_strategy: "smart".to_owned(),
        model_mappings: BTreeMap::from([
            ("gpt-5.4".to_owned(), "gpt-5.5".to_owned()),
            ("grok-latest".to_owned(), "grok-4.5".to_owned()),
        ]),
        values: gateway_admin::model::settings::RuntimeSettingsValues {
            codex_privacy_policy: Default::default(),
            request_location_enabled: false,
            request_location: Default::default(),
            refresh_margin_seconds,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            max_waiting_per_key: 0,
            max_waiting_per_account: 0,
            concurrency_wait_timeout_seconds: 30,
            openai_guardian_reserved_concurrency: 0,
            openai_account_affinity: gateway_core::account::AccountAffinity::Relaxed,
            max_account_rotations: 3,
            openai_session_affinity_ttl_hours: 24,
            responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
            smart_scheduling: gateway_core::account::SmartSchedulingConfig::default(),
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
            account_warmup_enabled: false,
            account_warmup_schedule_time: "08:00".to_owned(),
            account_warmup_model: None,
        },
    }
}

#[test]
fn runtime_settings_keep_account_rotation_global() {
    let settings = settings_with_margin(3_600);
    assert!(settings.validate().is_ok());
}

#[tokio::test]
async fn privacy_policy_upgrade_defaults_off_and_round_trips_in_snapshot() {
    use gateway_core::settings::privacy::*;
    use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
    let Some(database) = TestDatabase::create_through("privacy_policy", 25).await else {
        return;
    };
    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = repository.load_runtime_settings().await.unwrap();
    assert_eq!(
        before.values.codex_privacy_policy,
        CodexPrivacyPolicy::default()
    );
    let mut update = settings_with_margin(300);
    update.values.codex_privacy_policy = CodexPrivacyPolicy {
        enabled: true,
        on_error: PrivacyFailureMode::RejectRequest,
        rules: vec![PrivacyRule {
            id: "remove-auth".into(),
            name: "配置者控制".into(),
            enabled: true,
            scope: PrivacyScope::RequestHeader,
            selector: "authorization".into(),
            action: PrivacyAction::RemoveField,
            pattern: None,
            replacement: String::new(),
            value: serde_json::Value::Null,
            replace_all: true,
            case_insensitive: false,
            multi_line: false,
        }],
    };
    let expected = update.values.codex_privacy_policy.clone();
    repository.update_runtime_settings(update).await.unwrap();
    let after = repository.load_runtime_settings().await.unwrap();
    assert_eq!(after.values.codex_privacy_policy, expected);
    assert!(after.config_revision > before.config_revision);
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(snapshot.settings).unwrap()["codex_privacy_policy"],
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(
        before.values.codex_privacy_policy,
        CodexPrivacyPolicy::default()
    );
    database.close().await;
}

#[tokio::test]
async fn smart_settings_upgrade_preserves_selection_and_publishes_custom_config() {
    use gateway_core::account::SmartSchedulingConfig;
    use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
    let Some(database) = TestDatabase::create_through("smart_config", 18).await else {
        return;
    };
    // 升级前不能用包含新列的 Repository，直接写入旧版本已有字段
    sqlx::query("update runtime_settings set rotation_strategy = 'sticky', refresh_margin_seconds = 3600 where id = 1")
        .execute(&database.pool).await.unwrap();
    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = repository.load_runtime_settings().await.unwrap();
    assert_eq!(before.rotation_strategy, "sticky");
    // 0022 将仍为旧默认 3600 的行迁移到 300；管理员自定义值才会原样保留。
    assert_eq!(before.values.refresh_margin_seconds, 300);
    assert_eq!(
        before.values.smart_scheduling,
        SmartSchedulingConfig::default()
    );
    let mut update = settings_with_margin(3600);
    update.values.smart_scheduling =
        SmartSchedulingConfig::new([0.0, 2.0, 1.0, 0.5, 1.2, 2.3], true).unwrap();
    let expected = update.values.smart_scheduling;
    repository.update_runtime_settings(update).await.unwrap();
    let reloaded = repository.load_runtime_settings().await.unwrap();
    assert_eq!(reloaded.values.smart_scheduling, expected);
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    let snapshot_values = serde_json::to_value(&snapshot.settings).unwrap();
    assert_eq!(
        snapshot_values["smart_scheduling"],
        serde_json::to_value(expected).unwrap()
    );
    assert!(snapshot.config_revision > before.config_revision);
    assert_eq!(
        before.values.smart_scheduling,
        SmartSchedulingConfig::default()
    );
    database.close().await;
}

#[test]
fn runtime_settings_require_model_when_warmup_is_enabled() {
    let settings = {
        let runtime_settings_base = settings_with_margin(3_600);
        RuntimeSettingsUpdate {
            values: gateway_admin::model::settings::RuntimeSettingsValues {
                account_warmup_enabled: true,
                ..runtime_settings_base.values
            },
            ..runtime_settings_base
        }
    };
    assert!(settings.validate().is_err());
}

#[tokio::test]
async fn warmup_settings_round_trip_with_database_constraint() {
    let Some(database) = TestDatabase::create("warmup_settings").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let mut update = settings_with_margin(3_600);
    update.values.account_warmup_enabled = true;
    update.values.account_warmup_schedule_time = "08:00,13:00".to_owned();
    update.values.account_warmup_model = Some("test-model".to_owned());
    repository
        .update_runtime_settings(update)
        .await
        .expect("save warmup settings");

    let settings = repository
        .load_runtime_settings()
        .await
        .expect("load warmup settings");
    assert!(settings.values.account_warmup_enabled);
    assert_eq!(settings.values.account_warmup_schedule_time, "08:00,13:00");
    assert_eq!(
        settings.values.account_warmup_model.as_deref(),
        Some("test-model")
    );

    let error = sqlx::query("update runtime_settings set account_warmup_model = null where id = 1")
        .execute(&database.pool)
        .await
        .expect_err("enabled warmup requires a model");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23514")
    );
    database.close().await;
}

#[tokio::test]
async fn unlimited_default_account_concurrency_round_trips_without_relaxing_other_limits() {
    let Some(database) = TestDatabase::create("unlimited_default_concurrency").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let mut update = settings_with_margin(3_600);
    update.values.max_concurrent_per_account = 0;
    repository
        .update_runtime_settings(update)
        .await
        .expect("persist unlimited default");
    let settings = repository
        .load_runtime_settings()
        .await
        .expect("read unlimited default");
    assert_eq!(settings.values.max_concurrent_per_account, 0);
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

#[tokio::test]
async fn migrations_should_narrow_the_default_refresh_margin_to_the_codex_baseline() {
    let Some(database) = TestDatabase::create("refresh_margin_default").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    // 新装实例：0001 种子行携带旧默认 3600，0022 数据迁移统一收敛到 300。
    let settings = repository
        .load_runtime_settings()
        .await
        .expect("load settings");
    assert_eq!(settings.values.refresh_margin_seconds, 300);
    // 列默认值同步收窄，重建行不会回退到旧值；目录查询限定本测试 schema，
    // 同库并行测试里停留在旧迁移版本的 schema 仍保留 3600 旧默认，不能被读到。
    let column_default: String = sqlx::query_scalar(
        "select column_default from information_schema.columns \
             where table_schema = current_schema() \
               and table_name = 'runtime_settings' \
               and column_name = 'refresh_margin_seconds'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("read column default");
    assert_eq!(column_default, "300");
    database.close().await;
}

#[tokio::test]
async fn refresh_margin_migration_should_preserve_admin_customized_values() {
    // 升级语义：0021 之前部署的实例经 0022 迁移后，仅旧默认 3600 收敛到 300，
    // 管理员自定义值原样保留。
    let Some(database) = TestDatabase::create_through("refresh_margin_custom", 21).await else {
        return;
    };
    sqlx::query("update runtime_settings set refresh_margin_seconds = 1800 where id = 1")
        .execute(&database.pool)
        .await
        .expect("persist customized margin");
    super::TEST_MIGRATOR
        .run(&database.pool)
        .await
        .expect("apply remaining migrations");
    let settings = PgRuntimeSettingsRepository::new(database.pool.clone())
        .load_runtime_settings()
        .await
        .expect("load settings");
    assert_eq!(settings.values.refresh_margin_seconds, 1_800);
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
        {
            let runtime_settings_base = settings_with_margin(3_600);
            RuntimeSettingsUpdate {
                values: gateway_admin::model::settings::RuntimeSettingsValues {
                    account_auto_freeze_threshold: 1,
                    ..runtime_settings_base.values
                },
                ..runtime_settings_base
            }
        },
        {
            let runtime_settings_base = settings_with_margin(3_600);
            RuntimeSettingsUpdate {
                values: gateway_admin::model::settings::RuntimeSettingsValues {
                    account_auto_freeze_window_seconds: 59,
                    ..runtime_settings_base.values
                },
                ..runtime_settings_base
            }
        },
        {
            let runtime_settings_base = settings_with_margin(3_600);
            RuntimeSettingsUpdate {
                values: gateway_admin::model::settings::RuntimeSettingsValues {
                    account_auto_freeze_duration_seconds: 299,
                    ..runtime_settings_base.values
                },
                ..runtime_settings_base
            }
        },
        {
            let runtime_settings_base = settings_with_margin(3_600);
            RuntimeSettingsUpdate {
                values: gateway_admin::model::settings::RuntimeSettingsValues {
                    account_auto_freeze_probe_model: Some(" pad ".to_owned()),
                    ..runtime_settings_base.values
                },
                ..runtime_settings_base
            }
        },
    ] {
        assert!(update.validate().is_err());
    }
}

#[test]
fn runtime_settings_reject_non_semver_client_min() {
    let settings = {
        let runtime_settings_base = settings_with_margin(3_600);
        RuntimeSettingsUpdate {
            values: gateway_admin::model::settings::RuntimeSettingsValues {
                min_codex_cli_version: Some("v0.40.0".to_owned()),
                ..runtime_settings_base.values
            },
            ..runtime_settings_base
        }
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
    update.values.min_codex_desktop_version = Some("26.825.6671".to_owned());
    update.values.min_codex_cli_version = Some("0.40.0".to_owned());

    repository
        .update_runtime_settings(update)
        .await
        .expect("update client min versions");
    let settings = repository
        .load_runtime_settings()
        .await
        .expect("load client min versions");

    assert_eq!(
        settings.values.min_codex_desktop_version.as_deref(),
        Some("26.825.6671")
    );
    assert_eq!(
        settings.values.min_codex_cli_version.as_deref(),
        Some("0.40.0")
    );
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
            before.values.max_waiting_per_key,
            before.values.max_waiting_per_account,
            before.values.concurrency_wait_timeout_seconds,
            before.values.openai_guardian_reserved_concurrency,
        ),
        (0, 0, 30, 0)
    );
    let mut update = settings_with_margin(3600);
    update.values.max_waiting_per_key = 5;
    update.values.max_waiting_per_account = 7;
    update.values.concurrency_wait_timeout_seconds = 12;
    update.values.openai_guardian_reserved_concurrency = 1;
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();
    assert_eq!(
        (
            settings.values.max_waiting_per_key,
            settings.values.max_waiting_per_account,
            settings.values.concurrency_wait_timeout_seconds,
            settings.values.openai_guardian_reserved_concurrency,
        ),
        (5, 7, 12, 1)
    );
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    let snapshot_values = serde_json::to_value(&snapshot.settings).unwrap();
    assert_eq!(
        (
            snapshot_values["max_waiting_per_key"].as_u64().unwrap(),
            snapshot_values["max_waiting_per_account"].as_u64().unwrap(),
            snapshot_values["concurrency_wait_timeout_seconds"]
                .as_u64()
                .unwrap(),
            snapshot_values["openai_guardian_reserved_concurrency"]
                .as_u64()
                .unwrap(),
        ),
        (5, 7, 12, 1)
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
    assert_eq!(before.values.request_location, RequestLocation::default());
    assert!(!before.values.request_location_enabled);
    let mut update = settings_with_margin(3600);
    update.values.request_location = serde_json::from_value(serde_json::json!({"country":"JP", "region":" Tokyo ", "city":" Tokyo ", "timezone":"Asia/Tokyo"})).unwrap();
    update.values.request_location_enabled = true;
    update.values.account_auto_freeze_threshold = 17;
    update.values.account_auto_freeze_window_seconds = 900;
    update.values.account_auto_freeze_duration_seconds = 3_600;
    update.values.account_auto_freeze_probe_model = Some("gpt-5.5".to_owned());
    update.values.account_auto_freeze_adaptive_concurrency = false;
    let expected = update.values.request_location.clone().normalized().unwrap();
    let mut disabled = update.clone();
    disabled.values.request_location = expected.clone();
    disabled.values.request_location_enabled = false;
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    let snapshot_values = serde_json::to_value(&snapshot.settings).unwrap();
    assert_eq!(settings.values.request_location, expected);
    assert_eq!(
        snapshot_values["request_location"],
        serde_json::to_value(&expected).unwrap()
    );
    assert!(settings.values.request_location_enabled);
    assert!(
        snapshot_values["request_location_enabled"]
            .as_bool()
            .unwrap()
    );
    assert!(snapshot.config_revision > before.config_revision);
    repository.update_runtime_settings(disabled).await.unwrap();
    let disabled_settings = repository.load_runtime_settings().await.unwrap();
    assert!(!disabled_settings.values.request_location_enabled);
    assert_eq!(disabled_settings.values.request_location, expected);
    let disabled_snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    let disabled_snapshot_values = serde_json::to_value(&disabled_snapshot.settings).unwrap();
    assert!(
        !disabled_snapshot_values["request_location_enabled"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        disabled_snapshot_values["request_location"],
        serde_json::to_value(&expected).unwrap()
    );
    // 位置开关与自动冻结共用设置写入，切换位置不能覆盖冻结参数
    for saved in [&settings, &disabled_settings] {
        assert!(saved.values.account_auto_freeze_enabled);
        assert_eq!(saved.values.account_auto_freeze_threshold, 17);
        assert_eq!(saved.values.account_auto_freeze_window_seconds, 900);
        assert_eq!(saved.values.account_auto_freeze_duration_seconds, 3_600);
        assert!(saved.values.account_auto_freeze_probe_enabled);
        assert_eq!(
            saved.values.account_auto_freeze_probe_model.as_deref(),
            Some("gpt-5.5")
        );
        assert!(!saved.values.account_auto_freeze_adaptive_concurrency);
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
            .values
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
            .values
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
            .values
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
        before.values.responses_max_decompressed_body_bytes,
        64 * 1024 * 1024
    );
    let mut update = settings_with_margin(3600);
    update.values.responses_max_decompressed_body_bytes = 128 * 1024 * 1024;
    repository.update_runtime_settings(update).await.unwrap();
    let reloaded = PgRuntimeSettingsRepository::new(database.pool.clone())
        .load_runtime_settings()
        .await
        .unwrap();
    assert_eq!(
        reloaded.values.responses_max_decompressed_body_bytes,
        128 * 1024 * 1024
    );
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    let snapshot_values = serde_json::to_value(&snapshot.settings).unwrap();
    assert_eq!(
        snapshot_values["responses_max_decompressed_body_bytes"],
        reloaded.values.responses_max_decompressed_body_bytes
    );
    assert!(snapshot.config_revision > before.config_revision);
    for invalid in [0, u64::MAX] {
        let mut update = settings_with_margin(3600);
        update.values.responses_max_decompressed_body_bytes = invalid;
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
            .request_profiles
            .get(&provider),
        Some(&initial)
    );
    let mut update = settings_with_margin(3600);
    update
        .request_profile_updates
        .insert(provider.clone(), Some(document("edited")));
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
async fn request_profile_deletion_is_explicit_and_preserves_other_profiles() {
    use gateway_core::{
        account::OpaqueProviderData, provider_ports::ProviderRuntimePolicyPort,
        routing::ProviderKind,
    };
    let Some(database) = TestDatabase::create("request_profile_deletion").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let kept = ProviderKind::new("provider.kept").unwrap();
    let removed = ProviderKind::new("provider.removed").unwrap();
    let profile = |marker: &str| {
        OpaqueProviderData::new(
            serde_json::json!({"marker":marker})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    repository
        .initialize_request_profile(&kept, profile("kept"))
        .await
        .unwrap();
    repository
        .initialize_request_profile(&removed, profile("removed"))
        .await
        .unwrap();

    let mut update = settings_with_margin(3_600);
    update.request_profile_updates.insert(removed.clone(), None);
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();

    assert_eq!(
        (
            settings.request_profiles.get(&kept),
            settings.request_profiles.get(&removed),
        ),
        (Some(&profile("kept")), None),
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
    update.request_profile_updates.insert(
        ProviderKind::new("openai").unwrap(),
        Some(document("openai")),
    );
    update
        .request_profile_updates
        .insert(provider.clone(), Some(document("xai")));
    repository.update_runtime_settings(update).await.unwrap();
    let mut update = settings_with_margin(3600);
    update
        .request_profile_updates
        .insert(provider.clone(), Some(document("edited")));
    repository.update_runtime_settings(update).await.unwrap();
    let settings = repository.load_runtime_settings().await.unwrap();
    assert_eq!(
        settings
            .request_profiles
            .get(&ProviderKind::new("openai").unwrap()),
        Some(&document("openai"))
    );
    assert_eq!(
        settings.request_profiles.get(&provider),
        Some(&document("edited"))
    );
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
async fn request_profile_projection_is_revision_consistent_and_includes_key_overrides() {
    use gateway_core::{
        account::OpaqueProviderData,
        provider_ports::{ProviderRuntimePolicyPort, ProviderStoreErrorKind},
        routing::ProviderKind,
    };
    let Some(database) = TestDatabase::create("request_profile_projection").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let provider = ProviderKind::new("xai").unwrap();
    let document = |marker: &str| {
        OpaqueProviderData::new(
            serde_json::json!({"marker":marker})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    let mut update = settings_with_margin(3600);
    update
        .request_profile_updates
        .insert(provider.clone(), Some(document("global")));
    repository.update_runtime_settings(update).await.unwrap();
    sqlx::query(
        "insert into client_api_keys (
           id, name, key, enabled, max_concurrency, requests_per_minute,
           provider_request_profiles_json, created_at, updated_at
         ) values ($1, $2, $3, true, 0, 0, $4::jsonb, now(), now())",
    )
    .bind("key_profile_projection")
    .bind("profile projection")
    .bind("synthetic-profile-projection-secret")
    .bind(sqlx::types::Json(serde_json::json!({
        "xai":{"marker":"key"}
    })))
    .execute(&database.pool)
    .await
    .unwrap();
    let revision = repository
        .load_runtime_settings()
        .await
        .unwrap()
        .config_revision;
    let revision = gateway_core::routing::ConfigRevision::new(revision.get()).unwrap();
    let profiles = repository
        .load_request_profile_configurations(revision, &provider)
        .await
        .unwrap();
    assert_eq!(profiles.len(), 2);
    assert!(profiles.contains(&document("global")));
    assert!(profiles.contains(&document("key")));

    repository
        .update_runtime_settings(settings_with_margin(7200))
        .await
        .unwrap();
    let error = repository
        .load_request_profile_configurations(revision, &provider)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderStoreErrorKind::Conflict);
    database.close().await;
}

#[tokio::test]
async fn warmup_cursor_survives_restart_and_settings_updates_without_rewinding() {
    use gateway_core::{provider_ports::ProviderRuntimePolicyPort, time::DeploymentTimeZone};
    let Some(database) = TestDatabase::create("warmup_cursor").await else {
        return;
    };
    let zone = DeploymentTimeZone::default();
    let slot = zone
        .local(Utc::now())
        .date_naive()
        .and_hms_opt(8, 0, 0)
        .unwrap();
    let first = PgRuntimeSettingsRepository::new(database.pool.clone());
    let second = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = first.load_runtime_settings().await.unwrap();
    let (a, b) = tokio::join!(
        first.claim_warmup_slot(zone, slot),
        second.claim_warmup_slot(zone, slot)
    );
    assert_ne!(a.unwrap(), b.unwrap(), "the local minute is claimed once");
    let after = first.load_runtime_settings().await.unwrap();
    assert_eq!(after.config_revision, before.config_revision);
    assert_eq!(after.updated_at, before.updated_at);
    let restarted = PgRuntimeSettingsRepository::new(database.pool.clone());
    assert!(!restarted.claim_warmup_slot(zone, slot).await.unwrap());
    assert!(
        restarted
            .claim_warmup_slot(zone, slot + TimeDelta::hours(5))
            .await
            .unwrap()
    );
    assert!(
        !restarted.claim_warmup_slot(zone, slot).await.unwrap(),
        "another slot cannot erase prior deduplication"
    );
    restarted
        .update_runtime_settings(settings_with_margin(3600))
        .await
        .unwrap();
    assert!(!restarted.claim_warmup_slot(zone, slot).await.unwrap());
    assert!(
        !restarted
            .claim_warmup_slot(zone, slot - TimeDelta::hours(1))
            .await
            .unwrap(),
        "clock rollback cannot replay an earlier time slot"
    );
    assert!(
        restarted
            .claim_warmup_slot(zone, slot + TimeDelta::days(1))
            .await
            .unwrap()
    );
    database.close().await;
}

#[tokio::test]
async fn runtime_scheduling_upgrade_preserves_settings_without_a_warmup_table() {
    let Some(database) = TestDatabase::create_through("runtime_scheduling_upgrade", 20).await
    else {
        return;
    };
    sqlx::query(
        "update runtime_settings set rotation_strategy = 'sticky',
         account_warmup_enabled = true, account_warmup_schedule_time = '08:00,13:00',
         account_warmup_model = 'test-model' where id = 1",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
    let warmup_table: Option<String> =
        sqlx::query_scalar("select to_regclass('account_warmup_slots')::text")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(warmup_table.is_none());
    let cursor: Option<DateTime<Utc>> =
        sqlx::query_scalar("select account_warmup_cursor from runtime_settings where id = 1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(cursor.is_none());
    let settings = PgRuntimeSettingsRepository::new(database.pool.clone())
        .load_runtime_settings()
        .await
        .unwrap();
    assert_eq!(settings.rotation_strategy, "sticky");
    assert!(settings.values.account_warmup_enabled);
    assert_eq!(settings.values.account_warmup_schedule_time, "08:00,13:00");
    assert_eq!(
        settings.values.account_warmup_model.as_deref(),
        Some("test-model")
    );
    assert_eq!(settings.values.openai_guardian_reserved_concurrency, 0);
    database.close().await;
}

#[tokio::test]
async fn warmup_cursor_resolves_dst_and_deduplicates_across_timezones() {
    use gateway_core::{provider_ports::ProviderRuntimePolicyPort, time::DeploymentTimeZone};
    let Some(database) = TestDatabase::create("warmup_cursor_dst").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let zone: DeploymentTimeZone = "America/New_York".parse().unwrap();
    let slot = chrono::NaiveDate::from_ymd_opt(2026, 11, 1)
        .unwrap()
        .and_hms_opt(1, 30, 0)
        .unwrap();
    assert!(repository.claim_warmup_slot(zone, slot).await.unwrap());
    let cursor: DateTime<Utc> =
        sqlx::query_scalar("select account_warmup_cursor from runtime_settings where id = 1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        cursor,
        "2026-11-01T05:30:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert!(!repository.claim_warmup_slot(zone, slot).await.unwrap());
    // 回拨中的重复本地时刻取较早一次，不能误挡后一分的正常调度
    let next = slot + TimeDelta::minutes(1);
    assert!(repository.claim_warmup_slot(zone, next).await.unwrap());
    let utc: DeploymentTimeZone = "UTC".parse().unwrap();
    assert!(
        !repository
            .claim_warmup_slot(utc, zone.resolve_local(next).unwrap().naive_utc())
            .await
            .unwrap(),
        "changing the deployment timezone cannot reclaim the same instant"
    );
    let missing = chrono::NaiveDate::from_ymd_opt(2026, 3, 8)
        .unwrap()
        .and_hms_opt(2, 30, 0)
        .unwrap();
    assert!(repository.claim_warmup_slot(zone, missing).await.is_err());
    database.close().await;
}

#[tokio::test]
async fn account_affinity_upgrade_defaults_and_updates_reach_the_snapshot() {
    use gateway_core::account::AccountAffinity;
    use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
    let Some(database) = TestDatabase::create_through("account_affinity_upgrade", 22).await else {
        return;
    };
    sqlx::query("update runtime_settings set rotation_strategy = 'sticky', config_revision = config_revision + 1 where id = 1")
        .execute(&database.pool)
        .await
        .unwrap();
    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
    let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
    let before = repository.load_runtime_settings().await.unwrap();
    assert_eq!(before.rotation_strategy, "sticky");
    assert_eq!(
        before.values.openai_account_affinity,
        AccountAffinity::Relaxed
    );
    assert_eq!(before.values.max_account_rotations, 3);
    assert_eq!(before.values.openai_session_affinity_ttl_hours, 24);
    for (mode, budget, ttl) in [
        (AccountAffinity::Preferred, 3, 24),
        (AccountAffinity::Strict, 31, 168),
        (AccountAffinity::Relaxed, 0, 720),
    ] {
        let mut update = settings_with_margin(3600);
        update.values.openai_account_affinity = mode;
        update.values.max_account_rotations = budget;
        update.values.openai_session_affinity_ttl_hours = ttl;
        repository.update_runtime_settings(update).await.unwrap();
        let loaded = repository.load_runtime_settings().await.unwrap();
        assert_eq!(
            (
                loaded.values.openai_account_affinity,
                loaded.values.max_account_rotations
            ),
            (mode, budget)
        );
        let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone())
            .load_runtime_snapshot()
            .await
            .unwrap();
        let snapshot_values = serde_json::to_value(&snapshot.settings).unwrap();
        assert_eq!(
            (
                serde_json::from_value::<AccountAffinity>(
                    snapshot_values["openai_account_affinity"].clone()
                )
                .unwrap(),
                snapshot_values["max_account_rotations"].as_u64().unwrap()
            ),
            (mode, u64::from(budget))
        );
        assert!(snapshot.config_revision > before.config_revision);
        assert_eq!(loaded.values.openai_session_affinity_ttl_hours, ttl);
        assert_eq!(snapshot_values["openai_session_affinity_ttl_hours"], ttl);
    }
    let mut invalid = settings_with_margin(3600);
    invalid.values.max_account_rotations = 32;
    assert!(repository.update_runtime_settings(invalid).await.is_err());
    for sql in [
        "update runtime_settings set openai_session_affinity_ttl_hours = 0 where id = 1",
        "update runtime_settings set openai_session_affinity_ttl_hours = 721 where id = 1",
        "update runtime_settings set max_account_rotations = 32 where id = 1",
        "update runtime_settings set max_account_rotations = -1 where id = 1",
        "update runtime_settings set openai_account_affinity = 'unknown' where id = 1",
    ] {
        assert!(sqlx::query(sql).execute(&database.pool).await.is_err());
    }
    assert_eq!(
        repository
            .load_runtime_settings()
            .await
            .unwrap()
            .values
            .max_account_rotations,
        0
    );
    database.close().await;
}

#[tokio::test]
async fn preferred_affinity_migration_defaults_to_strict_and_preserves_saved_modes() {
    use gateway_core::account::AccountAffinity;
    let Some(fresh) = TestDatabase::create("affinity_default").await else {
        return;
    };
    let repository = PgRuntimeSettingsRepository::new(fresh.pool.clone());
    assert_eq!(
        repository
            .load_runtime_settings()
            .await
            .unwrap()
            .values
            .openai_account_affinity,
        AccountAffinity::Strict
    );
    fresh.close().await;

    for mode in [AccountAffinity::Relaxed, AccountAffinity::Strict] {
        let database = TestDatabase::create_through("affinity_preserve", 23)
            .await
            .unwrap();
        let repository = PgRuntimeSettingsRepository::new(database.pool.clone());
        // 升级前只写当时已有列，现行 Repository 依赖完整迁移后的 schema
        sqlx::query("update runtime_settings set openai_account_affinity = $1, config_revision = config_revision + 1, updated_at = now() where id = 1")
            .bind(match mode {
                AccountAffinity::Relaxed => "relaxed",
                _ => "strict",
            })
            .execute(&database.pool)
            .await
            .unwrap();
        super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
        assert_eq!(
            repository
                .load_runtime_settings()
                .await
                .unwrap()
                .values
                .openai_account_affinity,
            mode
        );
        database.close().await;
    }
}
