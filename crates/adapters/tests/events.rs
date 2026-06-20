use codex_proxy_adapters::sqlite::events::{EventLogFilter, SqliteEventLogStore};
use codex_proxy_core::events::model::{EventLevel, EventLog};
use codex_proxy_platform::storage::connect_sqlite;
use serde_json::json;

#[tokio::test]
async fn event_log_store_should_filter_and_cursor_page_events() {
    let (store, _dir) = event_log_store("event-logs.sqlite").await;
    let mut matching = EventLog::new("request", EventLevel::Error, "upstream timeout");
    matching.id = "log_matching".to_string();
    matching.request_id = Some("req_1".to_string());
    matching.route = Some("/v1/responses".to_string());
    store.append(&matching).await.unwrap();
    store
        .append(&EventLog::new(
            "request",
            EventLevel::Info,
            "upstream timeout",
        ))
        .await
        .unwrap();
    store
        .append(&EventLog::new(
            "account",
            EventLevel::Error,
            "upstream timeout",
        ))
        .await
        .unwrap();

    let page = store
        .list(
            EventLogFilter {
                kind: Some("request".to_string()),
                level: Some(EventLevel::Error),
                search: Some("timeout".to_string()),
                ..EventLogFilter::default()
            },
            None,
            1,
        )
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "log_matching");
    assert_eq!(page.items[0].request_id.as_deref(), Some("req_1"));
    assert_eq!(page.items[0].route.as_deref(), Some("/v1/responses"));
}

#[tokio::test]
async fn event_log_store_should_promote_diagnostic_metadata_fields() {
    let (store, _dir) = event_log_store("event-logs-diagnostics.sqlite").await;
    let mut event = EventLog::new("request", EventLevel::Error, "upstream failed");
    event.id = "log_diagnostic".to_string();
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
            EventLogFilter {
                transport: Some("websocket".to_string()),
                failure_class: Some("rate_limited".to_string()),
                upstream_status_code: Some(429),
                ..EventLogFilter::default()
            },
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "log_diagnostic");
    assert_eq!(page.items[0].attempt_index, Some(2));
    assert_eq!(page.items[0].response_id.as_deref(), Some("resp_1"));
    assert_eq!(
        page.items[0].upstream_request_id.as_deref(),
        Some("req_upstream_1")
    );
}

#[tokio::test]
async fn event_log_store_should_get_count_and_clear_events() {
    let (store, _dir) = event_log_store("event-logs-clear.sqlite").await;
    let mut event = EventLog::new("request", EventLevel::Warn, "detail");
    event.id = "log_detail".to_string();
    store.append(&event).await.unwrap();

    let loaded = store.get("log_detail").await.unwrap().unwrap();
    assert_eq!(loaded.id, "log_detail");
    assert_eq!(store.count().await.unwrap(), 1);
    assert_eq!(store.clear().await.unwrap(), 1);
    assert_eq!(store.count().await.unwrap(), 0);
}

async fn event_log_store(db_name: &str) -> (SqliteEventLogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    (SqliteEventLogStore::new(pool), dir)
}
