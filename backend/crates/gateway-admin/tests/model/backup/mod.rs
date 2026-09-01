//! 备份领域模型（model/backup）的纯逻辑测试。

use chrono::{TimeZone as _, Utc};
use secrecy::SecretString;

use gateway_admin::model::backup::{
    BackupSettings, BackupStatus, BackupStorageConfig, BackupTriggerKind, build_object_key,
};

#[test]
fn status_machine_rejects_invalid_transitions() {
    assert!(BackupStatus::Queued.allows_transition_to(BackupStatus::Dumping));
    assert!(BackupStatus::Dumping.allows_transition_to(BackupStatus::Uploading));
    assert!(BackupStatus::Uploading.allows_transition_to(BackupStatus::Completed));
    assert!(BackupStatus::Completed.allows_transition_to(BackupStatus::Deleting));
    assert!(!BackupStatus::Queued.allows_transition_to(BackupStatus::Completed));
    assert!(!BackupStatus::Completed.allows_transition_to(BackupStatus::Dumping));
    assert!(!BackupStatus::Deleting.allows_transition_to(BackupStatus::Failed));
    assert!(!BackupStatus::Deleting.allows_transition_to(BackupStatus::Completed));
}

#[test]
fn status_classification() {
    assert!(BackupStatus::Queued.is_active());
    assert!(BackupStatus::Uploading.is_active());
    assert!(!BackupStatus::Completed.is_active());
    assert!(BackupStatus::Completed.can_be_deleted());
    assert!(BackupStatus::Failed.can_be_deleted());
    assert!(!BackupStatus::Queued.can_be_deleted());
}

#[test]
fn object_key_shape() {
    let at = Utc.with_ymd_and_hms(2026, 8, 1, 12, 30, 0).unwrap();
    let key = build_object_key("production/backups", "backup_abc", at).unwrap();
    assert_eq!(
        key,
        "production/backups/2026/08/01/codex-proxy-rs_20260801_123000_abc.dump"
    );
}

#[test]
fn object_key_rejects_dangerous_prefixes() {
    let at = Utc::now();
    assert!(build_object_key("/leading", "backup_x", at).is_err());
    assert!(build_object_key("a/../b", "backup_x", at).is_err());
    assert!(build_object_key("a\\b", "backup_x", at).is_err());
    assert!(build_object_key("a\u{0001}b", "backup_x", at).is_err());
    assert!(build_object_key("", "backup_x", at).is_err());
    assert!(build_object_key("trailing//", "backup_x", at).is_ok());
}

#[test]
fn trigger_and_status_roundtrip() {
    for status in [
        BackupStatus::Queued,
        BackupStatus::Dumping,
        BackupStatus::Uploading,
        BackupStatus::Completed,
        BackupStatus::Failed,
        BackupStatus::Deleting,
    ] {
        assert_eq!(BackupStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(
        BackupTriggerKind::parse("manual"),
        Some(BackupTriggerKind::Manual)
    );
    assert_eq!(
        BackupTriggerKind::parse("scheduled"),
        Some(BackupTriggerKind::Scheduled)
    );
    assert_eq!(BackupTriggerKind::parse("unknown"), None);
    assert_eq!(BackupStatus::parse("skipped"), None);
    assert_eq!(BackupStatus::parse("deleted"), None);
    assert_eq!(BackupStatus::parse("unknown"), None);
}

#[test]
fn config_from_settings_redacts_secret_in_debug() {
    let mut settings = BackupSettings {
        storage_revision: 3,
        endpoint: Some("https://example.com".to_owned()),
        region: Some("auto".to_owned()),
        bucket: Some("b".to_owned()),
        access_key_id: Some("ak".to_owned()),
        secret_access_key: Some(SecretString::from("sk-secret")),
        prefix: Some("p".to_owned()),
        force_path_style: false,
        schedule_enabled: false,
        cron_expression: None,
        schedule_timezone: None,
        retention_days: 0,
        retention_count: 0,
        next_run_at: None,
        last_verified_at: None,
        updated_at: Utc::now(),
    };
    assert!(!format!("{:?}", settings).contains("sk-secret"));
    let config = BackupStorageConfig::from_settings(&settings).unwrap();
    assert!(!format!("{config:?}").contains("sk-secret"));
    settings.endpoint = None;
    assert!(BackupStorageConfig::from_settings(&settings).is_none());
}
