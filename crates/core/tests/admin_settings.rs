use std::collections::BTreeMap;

use codex_proxy_core::admin::settings::{
    AdminQuotaWarningThresholds, AdminSettings, AdminSettingsPatch, SettingsService,
};

#[test]
fn settings_service_should_apply_retained_settings_patch() {
    let mut current = AdminSettings {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: Some("high".to_string()),
        service_tier: Some("flex".to_string()),
        model_aliases: BTreeMap::from([("codex-fast".to_string(), "gpt-5.5".to_string())]),
        refresh_enabled: true,
        refresh_margin_seconds: 240,
        refresh_concurrency: 2,
        max_concurrent_per_account: 4,
        request_interval_ms: 50,
        rotation_strategy: "least_used".to_string(),
        tier_priority: vec!["team".to_string(), "plus".to_string()],
        quota_refresh_interval_minutes: 5,
        quota_warning_thresholds: AdminQuotaWarningThresholds {
            primary: vec![80, 90],
            secondary: vec![70, 95],
        },
        quota_skip_exhausted: true,
        logs_enabled: true,
        logs_capacity: 2_000,
        logs_capture_body: false,
        usage_history_retention_days: Some(30),
    };

    SettingsService::apply_patch(
        &mut current,
        AdminSettingsPatch {
            default_model: Some(" gpt-6 ".to_string()),
            default_reasoning_effort: Some("medium".to_string()),
            service_tier: Some("priority".to_string()),
            model_aliases: Some(BTreeMap::from([(
                "fast".to_string(),
                "gpt-6-fast".to_string(),
            )])),
            refresh_enabled: Some(false),
            refresh_margin_seconds: Some(180),
            refresh_concurrency: Some(3),
            max_concurrent_per_account: Some(5),
            request_interval_ms: Some(125),
            rotation_strategy: Some("round_robin".to_string()),
            tier_priority: Some(vec!["pro".to_string(), "plus".to_string()]),
            quota_refresh_interval_minutes: Some(15),
            quota_warning_thresholds: Some(AdminQuotaWarningThresholds {
                primary: vec![75, 90],
                secondary: vec![65, 95],
            }),
            quota_skip_exhausted: Some(false),
            logs_enabled: Some(false),
            logs_capacity: Some(3_000),
            logs_capture_body: Some(true),
            usage_history_retention_days: Some(60),
        },
    )
    .expect("settings patch should be valid");

    assert_eq!(current.default_model, "gpt-6");
    assert_eq!(current.rotation_strategy, "round_robin");
    assert_eq!(current.quota_warning_thresholds.primary, vec![75, 90]);
    assert_eq!(current.usage_history_retention_days, Some(60));
    assert!(!current.refresh_enabled);
    assert!(current.logs_capture_body);
}

#[test]
fn settings_service_should_reject_invalid_patch_values() {
    let mut current = AdminSettings::default();
    let error = SettingsService::apply_patch(
        &mut current,
        AdminSettingsPatch {
            rotation_strategy: Some("random".to_string()),
            ..AdminSettingsPatch::default()
        },
    )
    .expect_err("invalid rotation strategy should be rejected");

    assert_eq!(error.field(), "rotationStrategy");
}
