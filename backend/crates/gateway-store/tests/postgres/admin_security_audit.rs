use chrono::Utc;
use gateway_store::postgres::{AdminAuditActorKind, AdminAuditEvent};

#[tokio::test]
async fn password_change_commits_audit_atomically_and_rejects_concurrent_old_hash() {
    use gateway_store::postgres::{AdminSecurityAuditRepository, PgAdminSecurityAuditRepository};
    let Some(database) = super::TestDatabase::create("password_change").await else {
        return;
    };
    let repository = PgAdminSecurityAuditRepository::new(database.pool.clone());
    repository
        .create_password_hash_if_absent("admin", "old-test-hash")
        .await
        .unwrap();
    let audit = |id: &str| AdminAuditEvent {
        id: id.into(),
        actor_kind: AdminAuditActorKind::AdminSession,
        actor_admin_user_id: Some("admin".into()),
        actor_ref: "admin:admin".into(),
        admin_request_id: None,
        action: "admin.password_changed".into(),
        entity_kind: "admin_user".into(),
        entity_ref: "admin".into(),
        config_revision: None,
        changed_fields: vec!["password".into()],
        created_at: Utc::now(),
    };
    let (first, second) = tokio::join!(
        repository.change_password(
            "admin",
            "old-test-hash",
            "first-new-hash",
            audit("change-1")
        ),
        repository.change_password(
            "admin",
            "old-test-hash",
            "second-new-hash",
            audit("change-2")
        ),
    );
    assert_ne!(first.unwrap(), second.unwrap());
    let current = repository.password_hash("admin").await.unwrap().unwrap();
    assert!(current == "first-new-hash" || current == "second-new-hash");
    let count: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // 制造真实审计约束失败，确认密码更新随同事务回滚。
    let mut invalid_audit = audit("invalid-change");
    invalid_audit.actor_admin_user_id = Some("missing-admin".into());
    assert!(
        repository
            .change_password("admin", &current, "must-not-commit", invalid_audit)
            .await
            .is_err()
    );
    assert_eq!(
        repository.password_hash("admin").await.unwrap().as_deref(),
        Some(current.as_str())
    );
    database.close().await;
}

#[test]
fn audit_event_rejects_more_than_sixty_four_changed_fields() {
    let event = AdminAuditEvent {
        id: "audit-1".to_owned(),
        actor_kind: AdminAuditActorKind::System,
        actor_admin_user_id: None,
        actor_ref: "system".to_owned(),
        admin_request_id: None,
        action: "update".to_owned(),
        entity_kind: "settings".to_owned(),
        entity_ref: "1".to_owned(),
        config_revision: Some(2),
        changed_fields: (0..65).map(|index| format!("field-{index}")).collect(),
        created_at: Utc::now(),
    };
    assert!(event.validate().is_err());
}
