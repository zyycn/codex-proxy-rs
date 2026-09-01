//! BackupTask 执行链测试：驱动内存 fake 端口跑完整 dump/upload/verify/retention 流程。

use std::sync::Arc;

use chrono::{Duration, Utc};
use gateway_admin::{
    backup::task::BackupTask,
    model::backup::{
        BackupRecordSeed, BackupStatus, BackupTriggerKind, UpdateBackupScheduleCommand, code,
    },
    ports::backup::{BackupRepository, DatabaseDumpPort, DumpRequest},
};
use gateway_core::lifecycle::CancellationToken;

use super::support::{
    DUMP_CONTENT, FakeBackupRepository, FakeDumpPort, FakeObjectStore, backup_id,
    configured_settings, sha256_hex, system_context,
};

#[tokio::test]
async fn backup_task_runs_full_dump_upload_verify_pipeline() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new());
    let object_store = Arc::new(FakeObjectStore::new());

    let seed = BackupRecordSeed {
        id: backup_id("manual"),
        trigger_kind: BackupTriggerKind::Manual,
        scheduled_at: None,
        object_key: "codex/2026/08/01/manual.dump".to_owned(),
        expires_at: None,
    };
    repository
        .insert_backup_record(seed.clone())
        .await
        .expect("insert queued");

    let task = BackupTask::new(repository.clone(), dump.clone(), object_store.clone());
    task.run_cycle(&CancellationToken::new())
        .await
        .expect("run cycle");

    let records = repository.all_records();
    assert_eq!(records.len(), 1);
    let finished = &records[0];
    assert_eq!(finished.status, BackupStatus::Completed);
    assert_eq!(finished.size_bytes, Some(DUMP_CONTENT.len() as u64));
    assert_eq!(
        finished.sha256.as_deref(),
        Some(sha256_hex(DUMP_CONTENT).as_str())
    );

    // 对象已上传且内容与本地归档一致。
    let stored = object_store
        .object(&seed.object_key)
        .expect("uploaded object exists");
    assert_eq!(stored.as_slice(), DUMP_CONTENT);
}

#[tokio::test]
async fn backup_task_marks_failed_when_dump_port_fails() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new().fail_next_dump());
    let object_store = Arc::new(FakeObjectStore::new());

    let seed = BackupRecordSeed {
        id: backup_id("fail"),
        trigger_kind: BackupTriggerKind::Manual,
        scheduled_at: None,
        object_key: "codex/2026/08/01/fail.dump".to_owned(),
        expires_at: None,
    };
    repository
        .insert_backup_record(seed.clone())
        .await
        .expect("insert queued");

    let task = BackupTask::new(repository.clone(), dump.clone(), object_store.clone());
    task.run_cycle(&CancellationToken::new())
        .await
        .expect("run cycle");

    let records = repository.all_records();
    let failed = &records[0];
    assert_eq!(failed.status, BackupStatus::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("backup.pg_dump_failed"));
    assert!(failed.completed_at.is_some());
    // 归档未上传，对象不存在。
    assert!(object_store.object(&seed.object_key).is_none());
}

#[tokio::test]
async fn backup_task_marks_failed_with_the_upload_error() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new());
    let object_store =
        Arc::new(FakeObjectStore::new().fail_upload(code::S3_UPLOAD_FAILED, "对象存储拒绝上传"));
    let seed = BackupRecordSeed {
        id: backup_id("upload-failure"),
        trigger_kind: BackupTriggerKind::Manual,
        scheduled_at: None,
        object_key: "codex/2026/08/01/upload-failure.dump".to_owned(),
        expires_at: None,
    };
    repository
        .insert_backup_record(seed.clone())
        .await
        .expect("insert queued");

    let task = BackupTask::new(repository.clone(), dump, object_store);
    task.run_cycle(&CancellationToken::new())
        .await
        .expect("run cycle");

    let records = repository.all_records();
    let failed = &records[0];
    assert_eq!(failed.status, BackupStatus::Failed);
    assert_eq!(failed.error_code.as_deref(), Some(code::S3_UPLOAD_FAILED));
    assert_eq!(
        failed.error_message.as_deref(),
        Some("上传到对象存储失败：对象存储拒绝上传")
    );
}

#[tokio::test]
async fn backup_task_recovery_marks_failed_with_the_upload_error() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new());
    let object_store =
        Arc::new(FakeObjectStore::new().fail_upload(code::S3_UPLOAD_FAILED, "对象存储拒绝上传"));
    let seed = BackupRecordSeed {
        id: backup_id("recovered-upload-failure"),
        trigger_kind: BackupTriggerKind::Manual,
        scheduled_at: None,
        object_key: "codex/2026/08/01/recovered-upload-failure.dump".to_owned(),
        expires_at: None,
    };
    repository
        .insert_backup_record(seed.clone())
        .await
        .expect("insert queued");
    let cancellation = CancellationToken::new();
    dump.dump(DumpRequest {
        backup_id: seed.id.clone(),
        cancellation: cancellation.clone(),
    })
    .await
    .expect("stage dump");
    repository.force_status(&seed.id, BackupStatus::Dumping);

    let task = BackupTask::new(repository.clone(), dump, object_store);
    task.run_cycle(&cancellation).await.expect("run cycle");

    let records = repository.all_records();
    let failed = &records[0];
    assert_eq!(failed.status, BackupStatus::Failed);
    assert_eq!(failed.error_code.as_deref(), Some(code::S3_UPLOAD_FAILED));
    assert_eq!(
        failed.error_message.as_deref(),
        Some("上传到对象存储失败：对象存储拒绝上传")
    );
}

#[tokio::test]
async fn retention_cleans_expired_scheduled_backups() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new());
    let object_store = Arc::new(FakeObjectStore::new());

    // 造一条旧 completed 计划备份（超过 retention_days）与一条新备份。
    let old_seed = BackupRecordSeed {
        id: backup_id("old"),
        trigger_kind: BackupTriggerKind::Scheduled,
        scheduled_at: Some(Utc::now() - Duration::days(40)),
        object_key: "codex/2026/06/01/old.dump".to_owned(),
        expires_at: None,
    };
    let new_seed = BackupRecordSeed {
        id: backup_id("new"),
        trigger_kind: BackupTriggerKind::Scheduled,
        scheduled_at: Some(Utc::now() - Duration::days(1)),
        object_key: "codex/2026/07/01/new.dump".to_owned(),
        expires_at: None,
    };
    repository
        .insert_scheduled_record(old_seed.clone())
        .await
        .expect("insert old");
    // 旧备份 40 天前完成（释放活跃名额），再插入新备份。
    let now = Utc::now();
    repository.set_completed(&backup_id("old"), now - Duration::days(40));
    repository
        .insert_scheduled_record(new_seed.clone())
        .await
        .expect("insert new");
    // 新备份 1 天前完成。
    repository.set_completed(&backup_id("new"), now - Duration::days(1));
    // 两个对象都已上传。
    object_store
        .objects
        .lock()
        .expect("objects")
        .insert(old_seed.object_key.clone(), DUMP_CONTENT.to_vec());
    object_store
        .objects
        .lock()
        .expect("objects")
        .insert(new_seed.object_key.clone(), DUMP_CONTENT.to_vec());

    // 设置 retention_days = 30。
    repository
        .update_schedule_settings(
            UpdateBackupScheduleCommand {
                schedule_enabled: false,
                cron_expression: "0 2 * * *".to_owned(),
                schedule_timezone: "Asia/Shanghai".to_owned(),
                retention_days: 30,
                retention_count: 0,
            },
            None,
            &system_context(),
        )
        .await
        .expect("set retention");

    let task = BackupTask::new(repository.clone(), dump.clone(), object_store.clone());
    task.run_cycle(&CancellationToken::new())
        .await
        .expect("run cycle");

    // 旧备份被硬删除，对象被删除；新备份保留。
    let records = repository.all_records();
    let ids: Vec<_> = records.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(ids, vec![backup_id("new").as_str()]);
    assert!(object_store.object(&old_seed.object_key).is_none());
    assert!(object_store.object(&new_seed.object_key).is_some());
}
