use gateway_admin::model::provider_credentials::{
    AccountUsagePeriod, AuthorizationMutationTarget, AuthorizationOwnerBinding,
    PendingAuthorizationMutation, ProviderQuota, ProviderQuotaWindow, ProviderQuotaWindowRole,
    QuotaLocalUsageAttribution,
};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_core::account::ProviderAccountId;
use gateway_core::routing::ProviderKind;
use serde_json::{Value, json};

#[test]
fn usage_window_should_prefer_weekly_without_changing_dashboard_selection() {
    let quota = usage_quota(vec![
        usage_window("short", "shortTerm", 18_000),
        usage_window("month", "monthly", 30 * 86_400),
        usage_window("week", "shortTerm", 7 * 86_400),
    ]);

    assert_eq!(
        selected_usage_window(&quota),
        Some(("week", AccountUsagePeriod::Weekly))
    );
    assert_eq!(
        quota
            .representative_window()
            .map(|window| window.key.as_str()),
        Some("short")
    );
}

#[test]
fn usage_window_should_use_monthly_when_weekly_is_unavailable() {
    let mut model_week = usage_window("model-week", "shortTerm", 7 * 86_400);
    model_week.local_usage_attribution = QuotaLocalUsageAttribution::Unavailable;
    let mut incomplete_week = usage_window("incomplete-week", "shortTerm", 7 * 86_400);
    incomplete_week.reset_at = None;
    let mut unknown_usage = usage_window("unknown-week", "shortTerm", 7 * 86_400);
    unknown_usage.local_usage = None;
    let quota = usage_quota(vec![
        model_week,
        incomplete_week,
        unknown_usage,
        usage_window("month", "monthly", 30 * 86_400),
    ]);

    assert_eq!(
        selected_usage_window(&quota),
        Some(("month", AccountUsagePeriod::Monthly))
    );
}

#[test]
fn usage_window_should_not_fallback_to_short_daily_or_incomplete_windows() {
    let mut missing_duration = usage_window("unknown-month", "monthly", 30 * 86_400);
    missing_duration.window_seconds = None;
    let quota = usage_quota(vec![
        usage_window("short", "shortTerm", 18_000),
        usage_window("day", "shortTerm", 86_400),
        usage_window("invalid-month", "monthly", 86_400),
        missing_duration,
    ]);

    assert_eq!(quota.usage_window(), None);
}

#[test]
fn usage_window_should_preserve_provider_order_for_equal_periods() {
    let quota = usage_quota(vec![
        usage_window("first-week", "shortTerm", 7 * 86_400),
        usage_window("second-week", "shortTerm", 7 * 86_400),
    ]);

    assert_eq!(
        selected_usage_window(&quota),
        Some(("first-week", AccountUsagePeriod::Weekly))
    );
}

fn selected_usage_window(quota: &ProviderQuota) -> Option<(&str, AccountUsagePeriod)> {
    quota
        .usage_window()
        .map(|(window, period)| (window.key.as_str(), period))
}

fn usage_quota(windows: Vec<ProviderQuotaWindow>) -> ProviderQuota {
    ProviderQuota {
        plan_type: None,
        observed_at: None,
        refresh_token_expires_at: None,
        windows,
        limit_reached: false,
        provider_data: None,
    }
}

fn usage_window(key: &str, group: &str, seconds: u64) -> ProviderQuotaWindow {
    ProviderQuotaWindow {
        key: key.to_owned(),
        group: group.to_owned(),
        label: key.to_owned(),
        limit_id: None,
        limit_name: None,
        role: None,
        local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
        window_seconds: Some(seconds),
        used_percent: Some(50.0),
        reset_at: Some(chrono::Utc::now()),
        limit_reached: false,
        local_usage: Some(gateway_admin::model::accounts::AccountUsage {
            account_id: "acct_test".to_owned(),
            request_count: 0,
            success_count: 0,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            image_input_tokens: None,
            image_output_tokens: None,
            image_request_count: 0,
            image_request_failed_count: 0,
            total_tokens: None,
            cost_coverage: Default::default(),
            costs: Vec::new(),
            last_used_at: None,
            request_buckets: Vec::new(),
            models: Vec::new(),
        }),
        provider_data: None,
    }
}

#[test]
fn dashboard_quota_should_preserve_unknown_and_actual_window_facts() {
    let mut quota = ProviderQuota {
        plan_type: None,
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
                    Ok(expected.clone()),
                );
                let with_proxy = expected
                    .with_outbound_proxy(Some(
                        gateway_core::account::OutboundProxy::parse(
                            "http://user:secret@proxy.example:8080",
                        )
                        .unwrap(),
                    ))
                    .with_outbound_proxy_id(Some("proxy_saved".to_owned()));
                assert_eq!(
                    PendingAuthorizationMutation::from_storage_v1(Value::Object(
                        with_proxy.to_storage_v1()
                    )),
                    Ok(with_proxy)
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
