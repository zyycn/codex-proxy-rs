use codex_proxy_rs::{
    admin::monitoring::{
        usage_record_model::{UsageRecord, UsageRecordLevel},
        usage_record_store::{SqliteUsageRecordStore, UsageRecordFilter},
    },
    infra::database::connect_sqlite,
};
use serde_json::json;

#[tokio::test]
async fn usage_record_store_should_filter_and_cursor_page_events() {
    let (store, _dir) = usage_record_store("usage-records.sqlite").await;
    let mut matching = UsageRecord::new("request", UsageRecordLevel::Error, "upstream timeout");
    matching.id = "usage_matching".to_string();
    matching.request_id = Some("req_1".to_string());
    matching.route = Some("/v1/responses".to_string());
    store.append(&matching).await.unwrap();
    store
        .append(&UsageRecord::new(
            "request",
            UsageRecordLevel::Info,
            "upstream timeout",
        ))
        .await
        .unwrap();
    store
        .append(&UsageRecord::new(
            "account",
            UsageRecordLevel::Error,
            "upstream timeout",
        ))
        .await
        .unwrap();

    let page = store
        .list(
            UsageRecordFilter {
                kind: Some("request".to_string()),
                level: Some(UsageRecordLevel::Error),
                search: Some("timeout".to_string()),
                ..UsageRecordFilter::default()
            },
            None,
            1,
        )
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "usage_matching");
    assert_eq!(page.items[0].request_id.as_deref(), Some("req_1"));
    assert_eq!(page.items[0].route.as_deref(), Some("/v1/responses"));
}

#[tokio::test]
async fn usage_record_store_should_promote_diagnostic_metadata_fields() {
    let (store, _dir) = usage_record_store("usage-records-diagnostics.sqlite").await;
    let mut event = UsageRecord::new("request", UsageRecordLevel::Error, "upstream failed");
    event.id = "usage_diagnostic".to_string();
    event.metadata = json!({
        "transport": "websocket",
        "attemptIndex": 2,
        "upstreamStatus": 429,
        "failureClass": "rate_limited",
        "responseId": "resp_1",
        "upstreamRequestId": "req_upstream_1"
    });

    store.append(&event).await.unwrap();
    let page = store
        .list(
            UsageRecordFilter {
                transport: Some("websocket".to_string()),
                failure_class: Some("rate_limited".to_string()),
                upstream_status_code: Some(429),
                ..UsageRecordFilter::default()
            },
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "usage_diagnostic");
    assert_eq!(page.items[0].attempt_index, Some(2));
    assert_eq!(page.items[0].response_id.as_deref(), Some("resp_1"));
    assert_eq!(
        page.items[0].upstream_request_id.as_deref(),
        Some("req_upstream_1")
    );
}

#[tokio::test]
async fn usage_record_store_should_not_promote_cf_ray_as_upstream_request_id() {
    let (store, _dir) = usage_record_store("usage-records-cf-ray.sqlite").await;
    let mut event = UsageRecord::new("request", UsageRecordLevel::Error, "upstream failed");
    event.id = "usage_cf_ray".to_string();
    event.metadata = json!({
        "upstreamStatus": 403,
        "cfRay": "ray-1"
    });

    store.append(&event).await.unwrap();
    let loaded = store.get("usage_cf_ray").await.unwrap().unwrap();

    assert_eq!(loaded.upstream_request_id, None);
}

#[tokio::test]
async fn usage_record_store_should_get_and_clear_events() {
    let (store, _dir) = usage_record_store("usage-records-clear.sqlite").await;
    let mut event = UsageRecord::new("request", UsageRecordLevel::Warn, "detail");
    event.id = "usage_detail".to_string();
    store.append(&event).await.unwrap();

    let loaded = store.get("usage_detail").await.unwrap().unwrap();
    assert_eq!(loaded.id, "usage_detail");
    assert_eq!(store.clear().await.unwrap(), 1);
    let page = store
        .list(UsageRecordFilter::default(), None, 10)
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

async fn usage_record_store(db_name: &str) -> (SqliteUsageRecordStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    (SqliteUsageRecordStore::new(pool), dir)
}
