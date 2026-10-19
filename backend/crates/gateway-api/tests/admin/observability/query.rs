//! 查询 wire 校验的固定合同测试

use gateway_api::admin::observability::{
    DashboardQuery, DiagnosticDimension, DiagnosticsQuery, OpsQuery, TrendKind, UsageQuery,
    parse_attempt_index, parse_datetime, parse_status,
};
use serde_json::json;

#[test]
fn dashboard_query_should_parse_error_trends() {
    let query: DashboardQuery = serde_json::from_value(json!({"kind": "errors"})).unwrap();
    assert_eq!(query.trend_kind().unwrap(), TrendKind::Errors);
}

#[test]
fn dashboard_query_should_reject_unknown_trend_kind() {
    let query: DashboardQuery = serde_json::from_value(json!({"kind": "secret"})).unwrap();
    assert_eq!(query.trend_kind().unwrap_err().field(), "kind");
}

#[test]
fn usage_query_should_accept_page_number_and_size() {
    let query: UsageQuery = serde_json::from_value(json!({
        "currentPage": 129,
        "pageSize": 10
    }))
    .unwrap();
    assert_eq!(query.validate_pagination().unwrap(), (129, 10));
}

#[test]
fn observability_queries_should_reject_unknown_fields() {
    let query = json!({"unknownFilter": true});
    assert!(serde_json::from_value::<DashboardQuery>(query.clone()).is_err());
    assert!(serde_json::from_value::<UsageQuery>(query.clone()).is_err());
    assert!(serde_json::from_value::<OpsQuery>(query.clone()).is_err());
    assert!(serde_json::from_value::<DiagnosticsQuery>(query).is_err());
}

#[test]
fn ops_query_should_reject_page_size_above_maximum() {
    let query: OpsQuery = serde_json::from_value(json!({"pageSize": 101})).unwrap();
    assert_eq!(query.validate_pagination().unwrap_err().field(), "pageSize");
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
