//! 验证历史保留窗口的有效范围与各类数据的独立配置

use gateway_admin::model::retention::{RetentionPolicy, RetentionTarget};

#[test]
fn retention_policy_rejects_zero_for_each_target() {
    for (usage, ops, audit) in [(0, 1, 1), (1, 0, 1), (1, 1, 0)] {
        assert!(RetentionPolicy::try_new(usage, ops, audit).is_err());
    }
}

#[test]
fn request_retention_accepts_positive_windows_without_changing_other_targets() {
    for days in [1, 31, 32, 365, 3650, u32::MAX] {
        let policy = RetentionPolicy::try_new(days, 30, 90).unwrap();
        assert_eq!(policy.days(RetentionTarget::ModelRequests), days);
        assert_eq!(policy.days(RetentionTarget::OpsEvents), 30);
        assert_eq!(policy.days(RetentionTarget::AdminAuditEvents), 90);
    }
}
