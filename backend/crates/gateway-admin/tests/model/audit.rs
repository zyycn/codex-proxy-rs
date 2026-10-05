//! 验证管理审计意图保留操作身份，并按提交结果选择审计分类

use gateway_admin::model::{
    MutationActor, MutationContext,
    audit::{MutationAuditIntent, MutationAuditOperation},
    auth::AuditActorKind,
};

#[test]
fn mutation_intent_preserves_actor_request_and_committed_field_names() {
    let intent = MutationAuditIntent {
        operation: MutationAuditOperation::PluginInstanceConfigure,
        entity_ref: "instance-1",
    };
    for (actor, expected_kind, expected_user) in [
        (
            MutationActor::AdminSession {
                admin_user_id: "admin-1".to_owned(),
            },
            AuditActorKind::AdminSession,
            Some("admin-1"),
        ),
        (
            MutationActor::AdminApiKey,
            AuditActorKind::AdminApiKey,
            None,
        ),
        (MutationActor::System, AuditActorKind::System, None),
    ] {
        let event = intent.event(
            &MutationContext {
                actor,
                request_id: "request-1".to_owned(),
            },
            vec!["configuration_json".to_owned()],
        );
        assert_eq!(event.actor_kind, expected_kind);
        assert_eq!(event.actor_admin_user_id.as_deref(), expected_user);
        assert_eq!(event.request_id.as_deref(), Some("request-1"));
        assert_eq!(
            (event.action.as_str(), event.entity_kind.as_str()),
            ("configure", "plugin_instance")
        );
        assert_eq!(event.entity_ref, "instance-1");
        assert_eq!(event.changed_fields, ["configuration_json"]);
        assert_eq!(event.config_revision, None);
    }
}

#[test]
fn resulting_state_selects_the_existing_audit_classification() {
    for enabled in [true, false] {
        let action = if enabled { "enable" } else { "disable" };
        assert_eq!(
            MutationAuditOperation::AccountGroupEnabled { enabled }.classification(),
            (action, "account_group"),
        );
        assert_eq!(
            MutationAuditOperation::ClientApiKeyEnabled { enabled }.classification(),
            (action, "client_api_key"),
        );
    }
    assert_eq!(
        MutationAuditOperation::AdminApiKeyChanged { exists: false }.classification(),
        ("admin_api_key.delete", "runtime_settings"),
    );
    assert_eq!(
        MutationAuditOperation::AdminApiKeyChanged { exists: true }.classification(),
        ("admin_api_key.replace", "runtime_settings"),
    );
}
