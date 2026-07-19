use gateway_store::postgres::RuntimeRetentionSettings;

#[test]
fn retention_settings_preserve_independent_windows() {
    let settings = RuntimeRetentionSettings {
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
    };
    assert_eq!(settings.audit_retention_days, 90);
}
