//! 控制面替换事务的修订冲突与设置保留测试

use chrono::Utc;

use super::{TestDatabase, runtime_settings::settings_with_margin};

#[tokio::test]
async fn control_plane_replacement_commits_one_writer_per_revision_and_preserves_newer_settings() {
    use gateway_store::postgres::{
        AdminAuditActorKind, AdminAuditEvent, ControlPlaneReplacement, ControlPlaneRepository,
        PgControlPlaneRepository,
    };
    use gateway_store::{ConflictKind, StoreError};
    let Some(database) = TestDatabase::create("settings_compare_replace").await else {
        return;
    };
    let repository = PgControlPlaneRepository::new(database.pool.clone());
    let revision = repository
        .load_control_plane()
        .await
        .unwrap()
        .settings
        .config_revision;
    let replacement = |id: &str, margin| ControlPlaneReplacement {
        expected_revision: revision,
        settings: settings_with_margin(margin),
        audit: AdminAuditEvent {
            id: id.into(),
            actor_kind: AdminAuditActorKind::System,
            actor_admin_user_id: None,
            actor_ref: "system".into(),
            admin_request_id: Some(id.into()),
            action: "settings.replace".into(),
            entity_kind: "runtime_settings".into(),
            entity_ref: "1".into(),
            config_revision: None,
            changed_fields: vec!["refresh_margin_seconds".into()],
            created_at: Utc::now(),
        },
    };
    let mut customized = replacement("first", 1800);
    customized
        .settings
        .values
        .openai_oauth_guardian_review_model = false;
    customized.settings.values.openai_request_country =
        gateway_core::account::OpenAiRequestCountry::Custom {
            country: "CA".into(),
        };
    let (first, second) = tokio::join!(
        repository.replace_control_plane(customized),
        repository.replace_control_plane(replacement("second", 7200)),
    );
    let (saved, conflict) = match (first, second) {
        (Ok(saved), Err(error)) | (Err(error), Ok(saved)) => (saved.settings, error),
        other => panic!("expected exactly one successful transaction: {other:?}"),
    };
    assert!(matches!(
        conflict,
        StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        }
    ));
    let current = repository.load_control_plane().await.unwrap().settings;
    assert_eq!(current.config_revision.get(), revision.get() + 1);
    assert_eq!(current.values, saved.values);
    let audit_count: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where action = 'settings.replace'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audit_count, 1);
    // API Key 更新也推进相同版本，旧设置快照不能复活已经替换的 Key
    let mut key_audit = replacement("key", 3600).audit;
    key_audit.action = "settings.admin_key".into();
    repository
        .replace_admin_api_key(Some("new-test-key".into()), key_audit)
        .await
        .unwrap();
    let mut stale = replacement("stale", 3600);
    stale.expected_revision = saved.config_revision;
    assert!(matches!(
        repository.replace_control_plane(stale).await,
        Err(StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        })
    ));
    let mut fresh = replacement("fresh", 3600);
    fresh.expected_revision = repository
        .load_control_plane()
        .await
        .unwrap()
        .settings
        .config_revision;
    repository.replace_control_plane(fresh).await.unwrap();
    assert_eq!(
        repository
            .load_control_plane()
            .await
            .unwrap()
            .settings
            .admin_api_key
            .as_deref(),
        Some("new-test-key")
    );
    database.close().await;
}
