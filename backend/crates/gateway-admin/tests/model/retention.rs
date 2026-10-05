//! 验证历史保留窗口的有效范围与各类数据的独立配置

use gateway_admin::model::retention::{RetentionPolicy, RetentionTarget};

#[test]
fn retention_policy_rejects_invalid_windows_and_preserves_independent_windows() {
    for (usage, ops, audit) in [(30, 1, 1), (31, 0, 1), (31, 1, 0)] {
        assert!(RetentionPolicy::try_new(usage, ops, audit).is_err());
    }
    let policy = RetentionPolicy::try_new(31, 7, 90).unwrap();
    assert_eq!(policy.days(RetentionTarget::ModelRequests), 31);
    assert_eq!(policy.days(RetentionTarget::OpsEvents), 7);
    assert_eq!(policy.days(RetentionTarget::AdminAuditEvents), 90);
}
