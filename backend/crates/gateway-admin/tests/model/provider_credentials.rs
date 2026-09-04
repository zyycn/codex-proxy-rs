use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, PendingAuthorizationMutation,
    ProviderQuota, ProviderQuotaWindow, ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_core::account::ProviderAccountId;
use gateway_core::routing::ProviderKind;
use serde_json::{Value, json};

#[test]
fn dashboard_quota_should_preserve_unknown_and_actual_window_facts() {
    let mut quota = ProviderQuota {
        observed_at: None,
        refresh_token_expires_at: None,
        windows: Vec::new(),
        limit_reached: false,
        provider_data: None,
    };
    assert!(quota.representative_window().is_none());
    assert!(quota.representative_used_percent().is_none());

    let rolling = ProviderQuotaWindow {
        key: "provider_rolling".to_owned(),
        group: "requests".to_owned(),
        label: "Provider rolling window".to_owned(),
        limit_id: None,
        limit_name: None,
        role: Some(ProviderQuotaWindowRole::Primary),
        local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
        window_seconds: Some(86_400),
        used_percent: None,
        reset_at: None,
        limit_reached: false,
        local_usage: None,
        provider_data: None,
    };
    quota.windows.push(rolling.clone());
    assert_eq!(quota.representative_window(), Some(&rolling));
    assert!(quota.representative_used_percent().is_none());

    quota.windows[0].used_percent = Some(99.6);
    quota.apply_limit_reached_display();
    let actual = quota.representative_window().expect("actual window");
    assert_eq!(actual.used_percent, Some(99.6));
    assert!(
        !actual.limit_reached,
        "display rounding cannot exhaust quota"
    );
    assert_eq!(actual.window_seconds, Some(86_400));
    assert_eq!(actual.reset_at, None);

    quota.windows.insert(
        0,
        ProviderQuotaWindow {
            key: "model_specific".to_owned(),
            used_percent: Some(100.0),
            local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
            ..rolling
        },
    );
    assert_eq!(
        quota
            .representative_window()
            .map(|window| window.key.as_str()),
        Some("provider_rolling")
    );
}

#[test]
fn pending_mutation_v1_should_round_trip_all_targets_and_owners() {
    for provider in ["openai", "xai"] {
        for target in [
            AuthorizationMutationTarget::Create {
                name: "account".to_owned(),
            },
            AuthorizationMutationTarget::Reauthorize {
                account_id: ProviderAccountId::new("acct_1").expect("account id"),
            },
        ] {
            for actor in [
                MutationActor::AdminSession {
                    admin_user_id: "admin-1".to_owned(),
                },
                MutationActor::AdminApiKey,
                MutationActor::System,
            ] {
                let expected = PendingAuthorizationMutation::new(
                    ProviderKind::new(provider).expect("provider"),
                    target.clone(),
                    AuthorizationOwnerBinding::from_context(&MutationContext {
                        actor,
                        request_id: "request-1".to_owned(),
                    }),
                );
                assert_eq!(
                    PendingAuthorizationMutation::from_storage_v1(Value::Object(
                        expected.to_storage_v1()
                    )),
                    Ok(expected),
                );
            }
        }
    }
}

#[test]
fn pending_mutation_v1_should_keep_existing_storage_fields_and_reject_invalid_documents() {
    let legacy = json!({
        "provider_kind": "openai",
        "target": { "kind": "reauthorize", "account_id": "acct_1" },
        "owner": { "kind": "admin_session", "admin_user_id": "admin-1" },
        "started_request_id": "request-1"
    });
    let restored =
        PendingAuthorizationMutation::from_storage_v1(legacy.clone()).expect("v1 document");
    assert_eq!(Value::Object(restored.to_storage_v1()), legacy);
    for (pointer, invalid) in [
        ("/provider_kind", json!("")),
        ("/target/account_id", json!("")),
        ("/target/kind", json!("other")),
        ("/owner/kind", json!("other")),
        ("/started_request_id", json!(null)),
    ] {
        let mut document = legacy.clone();
        *document.pointer_mut(pointer).expect("field") = invalid;
        assert!(
            PendingAuthorizationMutation::from_storage_v1(document).is_err(),
            "{pointer}"
        );
    }
    let mut document = legacy;
    document["schema_version"] = json!(2);
    assert!(PendingAuthorizationMutation::from_storage_v1(document).is_err());
}
