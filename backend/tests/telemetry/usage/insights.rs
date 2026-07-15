use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::telemetry::{
    ops::{store::PgOpsErrorLogStore, types::OpsErrorLog},
    usage::{
        insights::{RequestHealthTimeBucket, health_timeline},
        store::PgUsageRecordStore,
        types::UsageRecord,
    },
};
use serde_json::json;

use crate::support::storage::init_test_db;

#[tokio::test]
async fn health_timeline_should_use_final_request_outcomes_and_keep_neutral_counts_separate() {
    let (pool, _guard) = init_test_db("usage-health-timeline-final-outcomes").await;
    let usage_store = PgUsageRecordStore::new(pool.clone());
    let error_store = PgOpsErrorLogStore::new(pool.clone());
    let start = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();

    error_store
        .append(&service_error(
            "ops_recovered",
            "req_recovered",
            start + Duration::minutes(1),
        ))
        .await
        .unwrap();
    usage_store
        .append(&success(
            "usage_recovered",
            "req_recovered",
            start + Duration::minutes(2),
        ))
        .await
        .unwrap();
    usage_store
        .append(&success(
            "usage_success",
            "req_success",
            start + Duration::minutes(3),
        ))
        .await
        .unwrap();

    for (id, minute) in [("ops_failed_first", 4), ("ops_failed_latest", 5)] {
        error_store
            .append(&service_error(
                id,
                "req_failed",
                start + Duration::minutes(minute),
            ))
            .await
            .unwrap();
    }

    let mut cancelled = service_error(
        "ops_cancelled",
        "req_cancelled",
        start + Duration::minutes(6),
    );
    cancelled.status_code = Some(499);
    cancelled.failure_class = None;
    cancelled.metadata = json!({ "terminal": "cancelled" });
    error_store.append(&cancelled).await.unwrap();

    let mut caller_error = service_error(
        "ops_caller_error",
        "req_caller_error",
        start + Duration::minutes(7),
    );
    caller_error.status_code = Some(400);
    caller_error.metadata = json!({ "failureSource": "client" });
    error_store.append(&caller_error).await.unwrap();

    let buckets = health_timeline(&pool, start, start + Duration::minutes(15))
        .await
        .unwrap();

    assert_eq!(
        buckets,
        vec![RequestHealthTimeBucket {
            bucket_start: start,
            success_requests: 2,
            failed_requests: 1,
            cancelled_requests: 1,
            caller_error_requests: 1,
        }]
    );
}

fn success(id: &str, request_id: &str, created_at: chrono::DateTime<Utc>) -> UsageRecord {
    let mut record = UsageRecord::new("v1.response", "completed", "acct_health", "gpt-5", 200);
    record.id = id.to_string();
    record.request_id = Some(request_id.to_string());
    record.created_at = created_at;
    record
}

fn service_error(id: &str, request_id: &str, created_at: chrono::DateTime<Utc>) -> OpsErrorLog {
    let mut error = OpsErrorLog::new("v1.response", "request failed");
    error.id = id.to_string();
    error.request_id = Some(request_id.to_string());
    error.status_code = Some(503);
    error.failure_class = Some("upstream_unavailable".to_string());
    error.created_at = created_at;
    error
}
