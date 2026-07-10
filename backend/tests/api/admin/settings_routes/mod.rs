pub(crate) mod routes;

use codex_proxy_rs::settings::{SettingsPatch, SettingsService, SettingsSnapshot};

#[test]
fn settings_service_should_apply_retained_settings_patch() {
    let mut current = SettingsSnapshot {
        max_concurrent_per_account: 4,
        request_interval_ms: 50,
        rotation_strategy: "smart".to_string(),
        ..SettingsSnapshot::default()
    };

    SettingsService::apply_patch(
        &mut current,
        SettingsPatch {
            model_aliases: Some(
                [
                    (" claude-sonnet ".to_string(), " gpt-5.5 ".to_string()),
                    ("openai-fast".to_string(), "openai:gpt-4o".to_string()),
                ]
                .into(),
            ),
            refresh_margin_seconds: Some(600),
            refresh_concurrency: Some(3),
            max_concurrent_per_account: Some(5),
            request_interval_ms: Some(125),
            rotation_strategy: Some("round_robin".to_string()),
        },
    )
    .expect("settings patch should be valid");

    assert_eq!(current.model_aliases["claude-sonnet"], "gpt-5.5");
    assert_eq!(current.model_aliases["openai-fast"], "openai:gpt-4o");
    assert_eq!(current.refresh_margin_seconds, 600);
    assert_eq!(current.refresh_concurrency, 3);
    assert_eq!(current.rotation_strategy, "round_robin");
    assert_eq!(current.max_concurrent_per_account, 5);
    assert_eq!(current.request_interval_ms, 125);
}

#[test]
fn settings_service_should_reject_invalid_patch_values() {
    let mut current = SettingsSnapshot::default();
    let error = SettingsService::apply_patch(
        &mut current,
        SettingsPatch {
            rotation_strategy: Some("random".to_string()),
            ..SettingsPatch::default()
        },
    )
    .expect_err("invalid rotation strategy should be rejected");

    assert_eq!(error.field(), "rotationStrategy");
}

#[test]
fn settings_service_should_reject_invalid_model_aliases() {
    let mut current = SettingsSnapshot::default();
    let error = SettingsService::apply_patch(
        &mut current,
        SettingsPatch {
            model_aliases: Some([("gpt-5.5".to_string(), "gpt-5.5".to_string())].into()),
            ..SettingsPatch::default()
        },
    )
    .expect_err("self-referencing model alias should be rejected");

    assert_eq!(error.field(), "modelAliases");
}
