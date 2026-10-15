//! 验证历史保留窗口的有效范围与各类数据的独立配置

use gateway_admin::model::retention::{RetentionPolicy, RetentionTarget};

#[test]
fn retention_policy_rejects_invalid_windows_and_preserves_independent_windows() {
    for (usage, ops, audit) in [
        (0, 1, 1),
        (32, 1, 1),
        (u32::MAX, 1, 1),
        (1, 0, 1),
        (1, 1, 0),
    ] {
        assert!(RetentionPolicy::try_new(usage, ops, audit).is_err());
    }
    let policy = RetentionPolicy::try_new(31, 7, 90).unwrap();
    assert_eq!(policy.days(RetentionTarget::ModelRequests), 31);
    assert_eq!(policy.days(RetentionTarget::OpsEvents), 7);
    assert_eq!(policy.days(RetentionTarget::AdminAuditEvents), 90);
}

#[test]
fn request_retention_accepts_short_windows_without_changing_other_targets() {
    for days in [1, 3, 7, 15, 30, 31] {
        let policy = RetentionPolicy::try_new(days, 30, 90).unwrap();
        assert_eq!(policy.days(RetentionTarget::ModelRequests), days);
        assert_eq!(policy.days(RetentionTarget::OpsEvents), 30);
        assert_eq!(policy.days(RetentionTarget::AdminAuditEvents), 90);
    }
}
