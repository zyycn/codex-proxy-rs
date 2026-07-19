use chrono::Utc;
use gateway_store::postgres::{AdminAuditActorKind, AdminAuditEvent};

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
