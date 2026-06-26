pub mod routes;

use codex_proxy_rs::config::settings::{AdminSettings, AdminSettingsPatch, SettingsService};

#[test]
fn settings_service_should_apply_retained_settings_patch() {
    let mut current = AdminSettings {
        default_model: "gpt-5.5".to_string(),
        default_reasoning_effort: Some("high".to_string()),
        max_concurrent_per_account: 4,
        request_interval_ms: 50,
        rotation_strategy: "least_used".to_string(),
        ..AdminSettings::default()
    };

    SettingsService::apply_patch(
        &mut current,
        AdminSettingsPatch {
            default_model: Some(" gpt-6 ".to_string()),
            max_concurrent_per_account: Some(5),
            request_interval_ms: Some(125),
            rotation_strategy: Some("round_robin".to_string()),
        },
    )
    .expect("settings patch should be valid");

    assert_eq!(current.default_model, "gpt-6");
    assert_eq!(current.rotation_strategy, "round_robin");
    assert_eq!(current.max_concurrent_per_account, 5);
    assert_eq!(current.request_interval_ms, 125);
    assert_eq!(current.default_reasoning_effort.as_deref(), Some("high"));
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
