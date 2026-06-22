use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::gateway::dispatch::session_affinity::{
    SessionAffinityEntry, SqliteSessionAffinityStore,
};
use codex_proxy_rs::infra::database::connect_sqlite;

#[tokio::test]
async fn session_affinity_store_should_restore_active_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("runtime-session-affinity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.expect("sqlite pool");
    let store = SqliteSessionAffinityStore::new(pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap();

    insert_account(&pool, "acct_restore").await;
    store
        .upsert(
            "resp_restore",
            &SessionAffinityEntry {
                account_id: "acct_restore".to_string(),
                conversation_id: "conv_restore".to_string(),
                turn_state: Some("turn_restore".to_string()),
                instructions_hash: Some("hash_restore".to_string()),
                input_tokens: Some(7),
                function_call_ids: vec!["call_restore".to_string()],
                variant_hash: Some("variant_restore".to_string()),
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
    assert_eq!(records[0].response_id, "resp_restore");
    assert_eq!(records[0].entry.account_id, "acct_restore");
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
