use codex_proxy_rs::{
    codex::logs::{
        event::{EventLevel, EventLog},
        repository::EventLogRepository,
    },
    platform::storage::db::connect_sqlite,
};

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
