//! 验证场景默认预算、插件声明隔离与无效输入边界

use std::time::Duration;

use gateway_plugin_sdk::{
    CallBudgetError, CallBudgetSource, CallBudgets, ResolvedCallBudgets, Stage,
};

#[test]
fn explicit_budgets_override_only_their_stage_without_a_global_ceiling() {
    let resolved = ResolvedCallBudgets::resolve(&CallBudgets::from([
        (Stage::Maintenance, 1_800_000),
        (Stage::CommandLine, 600_000),
        (Stage::Management, 250),
    ]))
    .unwrap();
    for (stage, millis, source) in [
        (Stage::Maintenance, 1_800_000, CallBudgetSource::Plugin),
        (Stage::CommandLine, 600_000, CallBudgetSource::Plugin),
        (Stage::Management, 250, CallBudgetSource::Plugin),
        (Stage::Registration, 5_000, CallBudgetSource::Default),
        (Stage::Authentication, 30_000, CallBudgetSource::Default),
        (Stage::Retry, 2_000, CallBudgetSource::Default),
        (Stage::Request, 120_000, CallBudgetSource::Default),
    ] {
        assert_eq!(resolved.budget(stage), Duration::from_millis(millis));
        assert_eq!(resolved.source(stage), source);
    }
    assert_eq!(
        ResolvedCallBudgets::resolve(&CallBudgets::new())
            .unwrap()
            .budget(Stage::Maintenance),
        Duration::from_secs(120)
    );
}

#[test]
fn invalid_budget_values_fail_before_use() {
    assert!(matches!(
        ResolvedCallBudgets::resolve(&CallBudgets::from([(Stage::Maintenance, 0)])),
        Err(CallBudgetError::Invalid {
            stage: Stage::Maintenance,
            milliseconds: 0
        })
    ));
    assert!(
        serde_json::from_str::<CallBudgets>(r#"{"maintenance":18446744073709551616}"#).is_err()
    );
}
