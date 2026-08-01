//! BackupService 用例测试：通过 AdminHarness 注入 fake 备份端口构造真实服务。

use std::sync::Arc;

use gateway_admin::model::backup::BackupStatus;
use gateway_admin::ports::backup::BackupStorePorts;

use crate::backup_runtime::{
    FakeAuthStore, FakeBackupRepository, FakeDumpPort, FakeObjectStore, configured_settings,
    system_context,
};

use super::AdminHarness;

#[tokio::test]
async fn backup_service_creates_downloads_and_deletes_with_audit() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new());
    let object_store = Arc::new(FakeObjectStore::new());
    let auth = Arc::new(FakeAuthStore::new());

    let services = AdminHarness::new()
        .auth(auth.clone())
        .backup(BackupStorePorts::new(
            repository.clone(),
            dump,
            object_store,
        ))
        .build()
        .await;

    // 创建手动备份 → queued + 审计。
    let created = services
        .backups()
        .create_backup(&system_context(), None)
        .await
        .expect("create backup");
    assert_eq!(created.status, BackupStatus::Queued);
    assert!(created.object_key.starts_with("codex/"));
    assert!(
        auth.audit_actions()
            .iter()
            .any(|action| action == "backup.created")
    );

    // 下载地址要求 completed 记录。
    repository.force_status(&created.id, BackupStatus::Completed);
    let download = services
        .backups()
        .download_url(&system_context(), &created.id)
        .await
        .expect("download url");
    assert!(download.url.starts_with("https://presigned.example.com/"));
    assert!(
        auth.audit_actions()
            .iter()
            .any(|action| action == "backup.download_url_created")
    );

    // 删除 → deleting + 审计。
    let deleting = services
        .backups()
        .delete_backup(&system_context(), &created.id)
        .await
        .expect("delete backup");
    assert_eq!(deleting.status, BackupStatus::Deleting);
    assert!(
        auth.audit_actions()
            .iter()
            .any(|action| action == "backup.delete_requested")
    );
}

#[tokio::test]
async fn backup_service_rejects_download_of_non_completed_record() {
    let repository = Arc::new(FakeBackupRepository::new(configured_settings()));
    let dump = Arc::new(FakeDumpPort::new());
    let object_store = Arc::new(FakeObjectStore::new());
    let auth = Arc::new(FakeAuthStore::new());

    let services = AdminHarness::new()
        .auth(auth.clone())
        .backup(BackupStorePorts::new(
            repository.clone(),
            dump,
            object_store,
        ))
        .build()
        .await;

    let created = services
        .backups()
        .create_backup(&system_context(), None)
        .await
        .expect("create backup");

    // queued 记录不能创建下载地址。
    let error = services
        .backups()
        .download_url(&system_context(), &created.id)
        .await
        .expect_err("non-completed record must be rejected");
    assert_eq!(error.kind(), gateway_admin::model::AdminErrorKind::Conflict);
}
