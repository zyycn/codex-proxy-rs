use chrono::{Duration, TimeZone, Utc};
use codex_proxy_adapters::sqlite::session_affinity::SqliteSessionAffinityStore;
use codex_proxy_core::serving::affinity::SessionAffinityEntry;
use codex_proxy_platform::storage::connect_sqlite;

#[tokio::test]
async fn session_affinity_store_should_upsert_and_list_active_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("session-affinity.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    insert_account(&pool, "acct_1").await;
    let store = SqliteSessionAffinityStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();

    store
        .upsert(
            "resp_1",
            &SessionAffinityEntry {
                account_id: "acct_1".to_string(),
                conversation_id: "conv_1".to_string(),
                turn_state: Some("turn_1".to_string()),
                instructions_hash: Some("hash_1".to_string()),
                input_tokens: Some(128),
                function_call_ids: vec!["call_a".to_string(), "call_b".to_string()],
                variant_hash: Some("variant_1".to_string()),
                created_at: now,
            },
            Duration::hours(4),
        )
        .await
        .expect("affinity should be stored");

    let records = store
        .list_active(now + Duration::minutes(1))
        .await
        .expect("active affinities should load");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].response_id, "resp_1");
    assert_eq!(records[0].entry.account_id, "acct_1");
    assert_eq!(records[0].entry.function_call_ids, vec!["call_a", "call_b"]);
}

#[tokio::test]
async fn session_affinity_store_should_delete_expired_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("session-affinity-cleanup.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    insert_account(&pool, "acct_1").await;
    insert_account(&pool, "acct_2").await;
    let store = SqliteSessionAffinityStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();

    store
        .upsert(
            "resp_expired",
            &SessionAffinityEntry {
                account_id: "acct_1".to_string(),
                conversation_id: "conv_1".to_string(),
                turn_state: None,
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                created_at: now - Duration::hours(5),
            },
            Duration::hours(1),
        )
        .await
        .expect("expired affinity should be stored");
    store
        .upsert(
            "resp_active",
            &SessionAffinityEntry {
                account_id: "acct_2".to_string(),
                conversation_id: "conv_2".to_string(),
                turn_state: None,
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                created_at: now,
            },
            Duration::hours(1),
        )
        .await
        .expect("active affinity should be stored");

    let deleted = store
        .delete_expired(now)
        .await
        .expect("expired affinities should be deleted");
    let records = store
        .list_active(now)
        .await
        .expect("active affinities should load");

    assert_eq!(deleted, 1);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].response_id, "resp_active");
}

async fn insert_account(pool: &sqlx::SqlitePool, id: &str) {
    sqlx::query(
        "insert into accounts (id, access_token_cipher, status, added_at, updated_at) values (?, 'cipher', 'active', '2026-06-18T12:00:00Z', '2026-06-18T12:00:00Z')",
    )
    .bind(id)
    .execute(pool)
    .await
    .expect("account should be inserted");
}
