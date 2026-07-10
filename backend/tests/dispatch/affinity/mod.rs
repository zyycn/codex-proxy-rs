use chrono::{Duration, Utc};
use codex_proxy_rs::dispatch::affinity::{RedisSessionAffinityStore, SessionAffinityEntry};

use crate::support::storage::create_test_redis;

#[tokio::test]
async fn redis_affinity_store_upserts_reads_and_forgets_entries() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-basic").await);
    let now = Utc::now();
    let entry = affinity("acct_1", "conv_1", Some("variant_1"), now);
    store
        .upsert("resp_1", &entry, Duration::hours(4))
        .await
        .unwrap();

    let loaded = store
        .get("resp_1", now + Duration::minutes(1), Duration::hours(4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded, entry);
    assert!(store.forget("resp_1").await.unwrap());
    assert!(store
        .get("resp_1", now, Duration::hours(4))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn redis_affinity_store_returns_latest_matching_conversation_variant() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-latest").await);
    let now = Utc::now();
    store
        .upsert(
            "resp_old",
            &affinity(
                "acct_old",
                "conv_shared",
                Some("variant_a"),
                now - Duration::minutes(2),
            ),
            Duration::hours(4),
        )
        .await
        .unwrap();
    store
        .upsert(
            "resp_new",
            &affinity(
                "acct_new",
                "conv_shared",
                Some("variant_a"),
                now - Duration::minutes(1),
            ),
            Duration::hours(4),
        )
        .await
        .unwrap();
    store
        .upsert(
            "resp_other",
            &affinity("acct_other", "conv_shared", Some("variant_b"), now),
            Duration::hours(4),
        )
        .await
        .unwrap();

    let (response_id, entry) = store
        .latest_by_conversation(
            "conv_shared",
            None,
            Some("variant_a"),
            now,
            Duration::hours(4),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response_id, "resp_new");
    assert_eq!(entry.account_id, "acct_new");
}

#[tokio::test]
async fn redis_affinity_store_forgets_all_entries_for_account() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-account").await);
    let now = Utc::now();
    for response_id in ["resp_1", "resp_2"] {
        store
            .upsert(
                response_id,
                &affinity("acct_delete", response_id, None, now),
                Duration::hours(4),
            )
            .await
            .unwrap();
    }

    assert_eq!(store.forget_account("acct_delete").await.unwrap(), 2);
    for response_id in ["resp_1", "resp_2"] {
        assert!(store
            .get(response_id, now, Duration::hours(4))
            .await
            .unwrap()
            .is_none());
    }
}

fn affinity(
    account_id: &str,
    conversation_id: &str,
    variant_hash: Option<&str>,
    created_at: chrono::DateTime<Utc>,
) -> SessionAffinityEntry {
    SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: conversation_id.to_string(),
        turn_state: Some(format!("turn_{conversation_id}")),
        instructions_hash: Some("instructions".to_string()),
        input_tokens: Some(7),
        function_call_ids: vec!["call_1".to_string()],
        variant_hash: variant_hash.map(ToString::to_string),
        created_at,
    }
}
