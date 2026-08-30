//! 查询 wire 校验的固定合同测试。

use gateway_api::admin::observability::{
    DashboardQuery, DiagnosticDimension, DiagnosticsQuery, OpsQuery, TrendKind, UsageQuery,
    parse_attempt_index, parse_datetime, parse_status,
};
use serde_json::json;

#[test]
fn dashboard_query_should_parse_terminal_trend_kinds() {
    let query: DashboardQuery = serde_json::from_value(json!({"kind": "errors"})).unwrap();
    assert_eq!(query.trend_kind().unwrap(), TrendKind::Errors);
}

#[test]
fn dashboard_query_should_reject_unknown_trend_kind() {
    let query: DashboardQuery = serde_json::from_value(json!({"kind": "secret"})).unwrap();
    assert_eq!(query.trend_kind().unwrap_err().field(), "kind");
}

#[test]
fn usage_query_should_bound_page_size_and_cursor() {
    let query: UsageQuery = serde_json::from_value(json!({
        "pageSize": 100,
        "cursor": "opaque"
    }))
    .unwrap();
    assert_eq!(query.validate_page_size().unwrap(), 100);
    assert!(query.validate_cursor().is_ok());
}

#[test]
fn usage_query_should_reject_removed_total_toggle() {
    assert!(serde_json::from_value::<UsageQuery>(json!({"includeTotal": true})).is_err());
}

#[test]
fn usage_query_should_reject_removed_offset_page_contract() {
    assert!(serde_json::from_value::<UsageQuery>(json!({"page": 1})).is_err());
}

#[test]
fn ops_query_should_reject_page_size_above_terminal_limit() {
    let query: OpsQuery = serde_json::from_value(json!({"pageSize": 101})).unwrap();
    assert_eq!(query.validate_page_size().unwrap_err().field(), "pageSize");
}

#[test]
fn diagnostics_query_should_keep_wire_dimension_name() {
    let query: DiagnosticsQuery =
        serde_json::from_value(json!({"dimension": "failure_class"})).unwrap();
    assert_eq!(query.dimension().unwrap(), DiagnosticDimension::Failure);
    assert_eq!(DiagnosticDimension::Failure.display_name(), "failureClass");
}

#[test]
fn scalar_query_parsers_should_reject_out_of_range_values_without_echoing_input() {
    assert_eq!(parse_status(Some(99)).unwrap_err().field(), "statusCode");
    assert_eq!(
        parse_attempt_index(Some(0)).unwrap_err().field(),
        "attemptIndex"
    );
    assert_eq!(
        parse_datetime(Some("not-a-time")).unwrap_err().field(),
        "timeRange"
    );
}
