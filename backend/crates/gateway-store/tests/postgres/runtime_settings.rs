use std::collections::BTreeMap;

use gateway_store::postgres::RuntimeSettingsUpdate;

#[test]
fn runtime_settings_keep_account_rotation_global() {
    let settings = RuntimeSettingsUpdate {
        admin_api_key: None,
        refresh_margin_seconds: 3_600,
        refresh_concurrency: 2,
        max_concurrent_per_account: 3,
        request_interval_ms: 50,
        rotation_strategy: "smart".to_owned(),
        provider_model_mappings: BTreeMap::from([
            (
                "openai".to_owned(),
                BTreeMap::from([("gpt-5.4".to_owned(), "gpt-5.5".to_owned())]),
            ),
            (
                "xai".to_owned(),
                BTreeMap::from([("grok-latest".to_owned(), "grok-4.5".to_owned())]),
            ),
        ]),
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
    };
    assert!(settings.validate().is_ok());
}

#[test]
fn runtime_settings_reject_invalid_provider_scoped_model_mapping() {
    let settings = RuntimeSettingsUpdate {
        admin_api_key: None,
        refresh_margin_seconds: 3_600,
        refresh_concurrency: 2,
        max_concurrent_per_account: 3,
        request_interval_ms: 50,
        rotation_strategy: "smart".to_owned(),
        provider_model_mappings: BTreeMap::from([(
            "Codex".to_owned(),
            BTreeMap::from([("gpt-5.4".to_owned(), "gpt-5.5".to_owned())]),
        )]),
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
    };

    assert!(settings.validate().is_err());
}
