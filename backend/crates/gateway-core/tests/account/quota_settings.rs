//! 额度桶配置校验与上限、预热的独立行为

use gateway_core::account::AccountQuotaSettings;
use serde_json::json;

#[test]
fn warmup_and_limit_can_be_configured_independently() {
    let settings: AccountQuotaSettings = serde_json::from_value(json!({
        "codex:18000s": {"warmup": {"model": "test-model"}},
        "codex:604800s": {"limitPercent": 95}
    }))
    .unwrap();
    assert!(settings.has_warmup());
    assert!(!settings.reached("codex:18000s", Some(100.0)));
    assert!(!settings.reached("codex:604800s", Some(94.9)));
    assert!(settings.reached("codex:604800s", Some(95.0)));
    assert!(!settings.reached("codex:604800s", None));
}

#[test]
fn malformed_bucket_settings_are_rejected() {
    for invalid in [
        json!(95),
        json!({"limitPercent": 0}),
        json!({"limitPercent": 101}),
        json!({"warmup": {}}),
        json!({"warmup": {"model": ""}}),
        json!({"warmup": {"model": " model"}}),
        json!({"warmup": {"model": "model\nname"}}),
        json!({"warmup": {"model": "x".repeat(129)}}),
        json!({"warmup": {"model": "model", "unexpected": true}}),
    ] {
        assert!(
            serde_json::from_value::<AccountQuotaSettings>(json!({"codex:18000s": invalid}))
                .is_err()
        );
    }
}
