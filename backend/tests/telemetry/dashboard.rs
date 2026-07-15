use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::{
    infra::time::china_day_start,
    telemetry::{
        dashboard::dashboard_health_timeline_data_at, usage::insights::RequestHealthTimeBucket,
    },
};
use serde_json::json;

#[test]
fn dashboard_health_timeline_should_classify_only_eligible_final_requests() {
    let now = Utc.with_ymd_and_hms(2026, 7, 15, 5, 0, 0).unwrap();
    let start = china_day_start(now);
    let records = vec![
        health_bucket(start, 0, 0, 7, 2),
        health_bucket(start + Duration::minutes(15), 9, 0, 0, 0),
        health_bucket(start + Duration::minutes(30), 0, 3, 0, 0),
        health_bucket(start + Duration::minutes(45), 98, 2, 0, 0),
        health_bucket(start + Duration::minutes(60), 99, 1, 0, 0),
    ];

    let value = serde_json::to_value(dashboard_health_timeline_data_at(&records, now)).unwrap();
    let points = value["points"].as_array().unwrap();

    assert_eq!(
        json!({
            "description": value["description"],
            "reliabilityDisplay": value["reliabilityDisplay"],
            "status": value["status"],
            "successRequests": value["successRequests"],
            "failedRequests": value["failedRequests"],
            "cancelledRequests": value["cancelledRequests"],
            "callerErrorRequests": value["callerErrorRequests"],
            "pointCount": points.len(),
            "statuses": points.iter().take(5).map(|point| &point["status"]).collect::<Vec<_>>(),
        }),
        json!({
            "description": "有效请求可用性",
            "reliabilityDisplay": "97.2%",
            "status": "unstable",
            "successRequests": 206,
            "failedRequests": 6,
            "cancelledRequests": 7,
            "callerErrorRequests": 2,
            "pointCount": 96,
            "statuses": ["no_data", "low_sample", "unavailable", "unstable", "stable"],
        })
    );
}

fn health_bucket(
    bucket_start: chrono::DateTime<Utc>,
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    caller_error_requests: u64,
) -> RequestHealthTimeBucket {
    RequestHealthTimeBucket {
        bucket_start,
        success_requests,
        failed_requests,
        cancelled_requests,
        caller_error_requests,
    }
}
