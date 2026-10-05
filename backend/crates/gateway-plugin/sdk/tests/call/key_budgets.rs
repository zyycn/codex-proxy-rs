//! 验证 Key 预算重置要求显式周期并拒绝额外权限字段

use gateway_plugin_sdk::call::key_budgets::{BudgetPeriod, ResetKeyBudgetRequest};
use serde_json::json;

#[test]
fn reset_requires_an_explicit_supported_period_and_rejects_extra_authority() {
    for period in [BudgetPeriod::Daily, BudgetPeriod::Weekly, BudgetPeriod::All] {
        let request = ResetKeyBudgetRequest {
            client_key_id: "key_1".into(),
            period,
        };
        assert_eq!(
            serde_json::from_value::<ResetKeyBudgetRequest>(
                serde_json::to_value(&request).unwrap()
            )
            .unwrap(),
            request
        );
    }
    for invalid in [
        json!({"client_key_id":"key_1"}),
        json!({"client_key_id":"key_1","period":"monthly"}),
        json!({"client_key_id":"key_1","period":"weekly","instance_id":"forged"}),
    ] {
        assert!(serde_json::from_value::<ResetKeyBudgetRequest>(invalid).is_err());
    }
}
