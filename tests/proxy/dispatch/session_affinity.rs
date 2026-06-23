use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::infra::database::connect_sqlite;
use codex_proxy_rs::proxy::dispatch::session_affinity::{
    SessionAffinityEntry, SqliteSessionAffinityStore,
};

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

#[tokio::test]
async fn session_affinity_store_should_forget_record_by_response_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("session-affinity-forget.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    insert_account(&pool, "acct_1").await;
    let store = SqliteSessionAffinityStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();

    store
        .upsert(
            "resp_forget",
            &SessionAffinityEntry {
                account_id: "acct_1".to_string(),
                conversation_id: "conv_forget".to_string(),
                turn_state: Some("turn_forget".to_string()),
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                created_at: now,
            },
            Duration::hours(1),
        )
        .await
        .expect("affinity should be stored");

    let forgotten = store
        .forget("resp_forget")
        .await
        .expect("affinity should be forgotten");
    let records = store
        .list_active(now)
        .await
        .expect("active affinities should load");

    assert!(forgotten);
    assert!(records.is_empty());
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

// ---------------------------------------------------------------------------
// In-memory SessionAffinityMap tests
// ---------------------------------------------------------------------------

use codex_proxy_rs::proxy::dispatch::session_affinity::{
    SessionAffinityMap, StoredSessionAffinity,
};

#[test]
fn session_affinity_map_should_restore_active_records_and_skip_expired_records() {
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();
    let mut map = SessionAffinityMap::new(Duration::hours(4));

    let restored = map.restore(
        vec![
            StoredSessionAffinity {
                response_id: "resp_active".to_string(),
                entry: SessionAffinityEntry {
                    account_id: "acct_active".to_string(),
                    conversation_id: "conv_active".to_string(),
                    turn_state: Some("turn_active".to_string()),
                    instructions_hash: Some("hash_active".to_string()),
                    input_tokens: Some(42),
                    function_call_ids: vec!["call_1".to_string()],
                    variant_hash: Some("variant_a".to_string()),
                    created_at: now - Duration::minutes(30),
                },
                expires_at: now + Duration::hours(1),
            },
            StoredSessionAffinity {
                response_id: "resp_expired".to_string(),
                entry: SessionAffinityEntry {
                    account_id: "acct_expired".to_string(),
                    conversation_id: "conv_expired".to_string(),
                    turn_state: None,
                    instructions_hash: None,
                    input_tokens: None,
                    function_call_ids: Vec::new(),
                    variant_hash: None,
                    created_at: now - Duration::hours(5),
                },
                expires_at: now + Duration::hours(1),
            },
        ],
        now,
    );

    assert_eq!(restored, 1);
    assert_eq!(
        map.lookup_account("resp_active", now),
        Some("acct_active".to_string())
    );
    assert_eq!(map.lookup_account("resp_expired", now), None);
}

#[test]
fn session_affinity_map_should_find_latest_response_by_conversation_and_variant() {
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();
    let mut map = SessionAffinityMap::new(Duration::hours(4));

    map.record(
        "resp_old".to_string(),
        SessionAffinityEntry {
            account_id: "acct_a".to_string(),
            conversation_id: "conv".to_string(),
            turn_state: Some("turn_old".to_string()),
            instructions_hash: None,
            input_tokens: None,
            function_call_ids: Vec::new(),
            variant_hash: Some("variant_a".to_string()),
            created_at: now - Duration::minutes(20),
        },
    );
    map.record(
        "resp_new".to_string(),
        SessionAffinityEntry {
            account_id: "acct_b".to_string(),
            conversation_id: "conv".to_string(),
            turn_state: Some("turn_new".to_string()),
            instructions_hash: None,
            input_tokens: None,
            function_call_ids: Vec::new(),
            variant_hash: Some("variant_a".to_string()),
            created_at: now - Duration::minutes(5),
        },
    );
    map.record(
        "resp_other_variant".to_string(),
        SessionAffinityEntry {
            account_id: "acct_c".to_string(),
            conversation_id: "conv".to_string(),
            turn_state: Some("turn_other".to_string()),
            instructions_hash: None,
            input_tokens: None,
            function_call_ids: Vec::new(),
            variant_hash: Some("variant_b".to_string()),
            created_at: now - Duration::minutes(1),
        },
    );

    assert_eq!(
        map.lookup_latest_response_by_conversation("conv", None, Some("variant_a"), now),
        Some("resp_new".to_string())
    );
    assert_eq!(
        map.lookup_turn_state("resp_new", now),
        Some("turn_new".to_string())
    );
}
