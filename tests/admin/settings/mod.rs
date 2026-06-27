pub mod routes;

use codex_proxy_rs::config::settings::{AdminSettings, AdminSettingsPatch, SettingsService};

#[test]
fn settings_service_should_apply_retained_settings_patch() {
    let mut current = AdminSettings {
        max_concurrent_per_account: 4,
        request_interval_ms: 50,
        rotation_strategy: "least_used".to_string(),
        ..AdminSettings::default()
    };

    SettingsService::apply_patch(
        &mut current,
        AdminSettingsPatch {
            model_aliases: Some(
                [
                    (" claude-sonnet ".to_string(), " gpt-5.5 ".to_string()),
                    ("openai-fast".to_string(), "openai:gpt-4o".to_string()),
                ]
                .into(),
            ),
            model_account_routes: Some(
                [(
                    " gpt-5.5 ".to_string(),
                    vec![" acct_a ".to_string(), "acct_b".to_string()],
                )]
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
    assert_eq!(
        current.model_account_routes["gpt-5.5"],
        vec!["acct_a".to_string(), "acct_b".to_string()]
    );
    assert_eq!(current.refresh_margin_seconds, 600);
    assert_eq!(current.refresh_concurrency, 3);
    assert_eq!(current.rotation_strategy, "round_robin");
    assert_eq!(current.max_concurrent_per_account, 5);
    assert_eq!(current.request_interval_ms, 125);
}

#[test]
fn settings_service_should_reject_invalid_model_account_routes() {
    let mut current = AdminSettings::default();
    let error = SettingsService::apply_patch(
        &mut current,
        AdminSettingsPatch {
            model_account_routes: Some(
                [(
                    "gpt-5.5".to_string(),
                    vec!["acct_a".to_string(), "acct_a".to_string()],
                )]
                .into(),
            ),
            ..AdminSettingsPatch::default()
        },
    )
    .expect_err("duplicate model route account should be rejected");

    assert_eq!(error.field(), "modelAccountRoutes");
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

#[test]
fn settings_service_should_reject_invalid_model_aliases() {
    let mut current = AdminSettings::default();
    let error = SettingsService::apply_patch(
        &mut current,
        AdminSettingsPatch {
            model_aliases: Some([("gpt-5.5".to_string(), "gpt-5.5".to_string())].into()),
            ..AdminSettingsPatch::default()
        },
    )
    .expect_err("self-referencing model alias should be rejected");

    assert_eq!(error.field(), "modelAliases");
}
