use codex_proxy_rs::{
    codex::events::{
        event::{EventLevel, EventLog},
        repository::EventLogRepository,
        service::LogService,
    },
    config::LoggingConfig,
    platform::storage::db::connect_sqlite,
};
use serde_json::json;

#[tokio::test]
async fn event_logs_are_cursor_paginated() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("logs.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = EventLogRepository::new(pool);

    for idx in 0..3 {
        repo.insert(EventLog::new(
            "request",
            EventLevel::Info,
            format!("event {idx}"),
        ))
        .await
        .unwrap();
    }

    let first = repo.list(None, 2).await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = repo.list(first.next_cursor, 2).await.unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn log_service_should_skip_record_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("logs-disabled.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let service = LogService::new(
        logging_config(false, 10, false),
        Some(EventLogRepository::new(pool.clone())),
    );

    let recorded = service
        .record(EventLog::new("request", EventLevel::Info, "disabled"))
        .await
        .unwrap();

    let stored_count: (i64,) = sqlx::query_as("select count(*) from event_logs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!recorded);
    assert_eq!(stored_count.0, 0);
}

#[tokio::test]
async fn log_service_should_trim_to_capacity_after_record() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("logs-capacity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let service = LogService::new(
        logging_config(true, 1, false),
        Some(EventLogRepository::new(pool.clone())),
    );
    let mut first = EventLog::new("request", EventLevel::Info, "first");
    first.created_at = "2026-06-14T00:00:00Z".to_string();
    let mut second = EventLog::new("request", EventLevel::Info, "second");
    second.created_at = "2026-06-14T00:00:01Z".to_string();

    service.record(first).await.unwrap();
    service.record(second).await.unwrap();

    let messages: Vec<(String,)> =
        sqlx::query_as("select message from event_logs order by created_at desc")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(messages, vec![("second".to_string(),)]);
}

#[tokio::test]
async fn log_service_should_remove_body_metadata_when_capture_body_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("logs-body-policy.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let service = LogService::new(
        logging_config(true, 10, false),
        Some(EventLogRepository::new(pool.clone())),
    );
    let mut event = EventLog::new("request", EventLevel::Info, "body");
    event.metadata = json!({
        "body": "secret",
        "requestBody": "secret",
        "responseBody": "secret",
        "safe": "visible"
    });

    service.record(event).await.unwrap();

    let metadata_json: (String,) = sqlx::query_as("select metadata_json from event_logs")
        .fetch_one(&pool)
        .await
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata_json.0).unwrap();
    assert_eq!(metadata, json!({ "safe": "visible" }));
}

fn logging_config(enabled: bool, capacity: u32, capture_body: bool) -> LoggingConfig {
    LoggingConfig {
        directory: "logs".to_string(),
        retention_days: 14,
        enabled,
        capacity,
        capture_body,
    }
}
