use chrono::{Duration, Utc};
use codex_proxy_rs::dispatch::affinity::{
    RedisSessionAffinityStore, ResponseReplaySnapshot, SessionAffinityEntry, MAX_REPLAY_DEPTH,
    MAX_REPLAY_SESSION_BYTES,
};
use redis::AsyncCommands;
use serde_json::json;

use crate::support::storage::create_test_redis;

#[tokio::test]
async fn redis_affinity_store_upserts_reads_and_forgets_entries() {
    let redis = create_test_redis("affinity-basic").await;
    let store = RedisSessionAffinityStore::new(redis.clone());
    let now = Utc::now();
    let entry = affinity("acct_1", "conv_1", Some("variant_1"), now);
    store
        .upsert("resp_1", &entry, Duration::hours(4))
        .await
        .unwrap();
    let mut connection = redis.manager();
    let ttl_seconds: i64 = connection
        .ttl(redis.key("affinity:v2:resp:resp_1"))
        .await
        .unwrap();
    assert!((3 * 60 * 60..=4 * 60 * 60).contains(&ttl_seconds));

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

#[tokio::test]
async fn redis_affinity_store_should_keep_replay_growth_linear_across_turns() {
    let redis = create_test_redis("affinity-replay-linear").await;
    let store = RedisSessionAffinityStore::new(redis.clone());
    let now = Utc::now();
    let mut parent_response_id = None;
    let mut total_bytes = 0;
    for turn in 0..16u16 {
        let response_id = format!("resp_{turn}");
        let turn_input = vec![json!({"role": "user", "content": format!("input-{turn}")})];
        let turn_output = vec![json!({"type": "message", "content": format!("output-{turn}")})];
        let node_bytes = replay_node_bytes(&turn_input, &turn_output);
        total_bytes += node_bytes;
        let mut entry = affinity(
            "acct_linear",
            "conv_linear",
            None,
            now + Duration::milliseconds(i64::from(turn)),
        );
        entry.replay = Some(ResponseReplaySnapshot {
            parent_response_id: parent_response_id.clone(),
            turn_input,
            turn_output,
            depth: turn + 1,
            total_bytes,
        });
        store
            .upsert(&response_id, &entry, Duration::hours(4))
            .await
            .unwrap();
        parent_response_id = Some(response_id);
    }

    let head = store
        .get("resp_15", now, Duration::hours(4))
        .await
        .unwrap()
        .unwrap();
    let replay = store
        .replay_input(
            "resp_15",
            &head,
            now,
            Duration::hours(4),
            MAX_REPLAY_DEPTH,
            MAX_REPLAY_SESSION_BYTES,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay.len(), 32);
    assert_eq!(replay[0]["content"], "input-0");
    assert_eq!(replay[31]["content"], "output-15");

    let mut connection = redis.manager();
    let head_json: String = connection
        .get(redis.key("affinity:v2:resp:resp_15"))
        .await
        .unwrap();
    assert!(head_json.contains("input-15"));
    assert!(!head_json.contains("input-0"));
    assert!(head_json.len() < 1_024);
}

#[tokio::test]
async fn redis_affinity_store_should_reject_replay_metadata_over_capacity() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-replay-limit").await);
    let now = Utc::now();
    let mut entry = affinity("acct_limit", "conv_limit", None, now);
    entry.replay = Some(ResponseReplaySnapshot {
        parent_response_id: None,
        turn_input: vec![json!({"role": "user", "content": "small"})],
        turn_output: Vec::new(),
        depth: 1,
        total_bytes: MAX_REPLAY_SESSION_BYTES + 1,
    });
    store
        .upsert("resp_limit", &entry, Duration::hours(4))
        .await
        .unwrap();

    assert!(store
        .replay_input(
            "resp_limit",
            &entry,
            now,
            Duration::hours(4),
            MAX_REPLAY_DEPTH,
            MAX_REPLAY_SESSION_BYTES,
        )
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn redis_affinity_store_should_use_pruned_account_zset_index() {
    let redis = create_test_redis("affinity-account-zset").await;
    let store = RedisSessionAffinityStore::new(redis.clone());
    let now = Utc::now();
    let account_key = redis.key("affinity:v2:account:acct_zset");
    let mut connection = redis.manager();
    let _: usize = connection
        .zadd(
            &account_key,
            "resp_expired",
            (now - Duration::hours(2)).timestamp_millis(),
        )
        .await
        .unwrap();

    store
        .upsert(
            "resp_current",
            &affinity("acct_zset", "conv_zset", None, now),
            Duration::hours(1),
        )
        .await
        .unwrap();

    let key_type: String = redis::cmd("TYPE")
        .arg(&account_key)
        .query_async(&mut connection)
        .await
        .unwrap();
    let members: Vec<String> = connection.zrange(&account_key, 0, -1).await.unwrap();
    assert_eq!(key_type, "zset");
    assert_eq!(members, vec!["resp_current"]);
}

#[tokio::test]
async fn redis_affinity_store_should_not_persist_already_expired_entries() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-expired-write").await);
    let now = Utc::now();
    store
        .upsert(
            "resp_expired",
            &affinity(
                "acct_expired",
                "conv_expired",
                None,
                now - Duration::hours(2),
            ),
            Duration::hours(1),
        )
        .await
        .unwrap();
    assert!(store
        .get("resp_expired", now, Duration::hours(1))
        .await
        .unwrap()
        .is_none());
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
        continuation_scope: codex_proxy_rs::upstream::openai::protocol::responses::PreviousResponseScope::ConnectionLocal,
        replay: None,
        created_at,
    }
}

fn replay_node_bytes(input: &[serde_json::Value], output: &[serde_json::Value]) -> u64 {
    serde_json::to_vec(&(input, output)).unwrap().len() as u64
}
