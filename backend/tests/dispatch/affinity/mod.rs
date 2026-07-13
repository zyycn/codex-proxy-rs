use chrono::{Duration, Utc};
use codex_proxy_rs::dispatch::affinity::{
    CyberPolicyFailureSnapshot, MAX_CONVERSATION_AFFINITIES, RedisSessionAffinityStore,
    SessionAffinityEntry,
};
use redis::AsyncCommands;

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
        .ttl(redis.key("affinity:v3:resp:resp_1"))
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
    assert!(
        store
            .get("resp_1", now, Duration::hours(4))
            .await
            .unwrap()
            .is_none()
    );
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
async fn redis_affinity_store_forgets_account_entries_in_one_operation() {
    let redis = create_test_redis("affinity-account").await;
    let store = RedisSessionAffinityStore::new(redis.clone());
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

    let bytes_before: u64 = redis::cmd("GET")
        .arg(redis.key("affinity:v3:global:bytes"))
        .query_async(&mut redis.manager())
        .await
        .unwrap();
    assert!(bytes_before > 0);
    assert_eq!(store.forget_account("acct_delete").await.unwrap(), 2);

    for response_id in ["resp_1", "resp_2"] {
        assert!(
            store
                .get(response_id, now, Duration::hours(4))
                .await
                .unwrap()
                .is_none()
        );
    }
    let bytes_after: Option<u64> = redis::cmd("GET")
        .arg(redis.key("affinity:v3:global:bytes"))
        .query_async(&mut redis.manager())
        .await
        .unwrap();
    assert!(bytes_after.is_none());
}

#[tokio::test]
async fn redis_affinity_store_caps_active_conversation_index() {
    let redis = create_test_redis("affinity-conversation-cap").await;
    let store = RedisSessionAffinityStore::new(redis.clone());
    let now = Utc::now();
    for index in 0..=MAX_CONVERSATION_AFFINITIES {
        store
            .upsert(
                &format!("resp_cap_{index}"),
                &affinity(
                    "acct_cap",
                    "conv_cap",
                    None,
                    now + Duration::milliseconds(index as i64),
                ),
                Duration::hours(4),
            )
            .await
            .unwrap();
    }

    let count: usize = redis::cmd("ZCARD")
        .arg(redis.key("affinity:v3:conv:conv_cap"))
        .query_async(&mut redis.manager())
        .await
        .unwrap();
    assert_eq!(count, MAX_CONVERSATION_AFFINITIES);
    assert!(
        store
            .get("resp_cap_0", now, Duration::hours(4))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn redis_affinity_store_prunes_expired_account_index_members_lazily() {
    let redis = create_test_redis("affinity-account-zset").await;
    let store = RedisSessionAffinityStore::new(redis.clone());
    let now = Utc::now();
    let account_key = redis.key("affinity:v3:account:acct_zset");
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
async fn redis_affinity_store_does_not_persist_expired_entries() {
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
    assert!(
        store
            .get("resp_expired", now, Duration::hours(1))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn redis_affinity_store_rejects_oversized_metadata() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-metadata-limit").await);
    let now = Utc::now();
    let mut entry = affinity("acct_limit", "conv_limit", None, now);
    entry.function_call_ids = vec!["x".repeat(64 * 1024)];

    let error = store
        .upsert("resp_limit", &entry, Duration::hours(4))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("metadata is too large"));
}

#[tokio::test]
async fn redis_affinity_store_caps_cyber_policy_failures_atomically() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-cyber-cap").await);
    for index in 1..=4 {
        let failure = CyberPolicyFailureSnapshot {
            account_id: format!("acct_{index}"),
            event: "error".to_string(),
            message: format!("blocked by account {index}"),
            upstream_code: Some("cyber_policy".to_string()),
        };
        store
            .record_cyber_policy_failure("session-hash", &failure, 3, Duration::hours(1))
            .await
            .unwrap();
    }

    let state = store
        .cyber_policy_state("session-hash")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.failed_account_ids, vec!["acct_1", "acct_2", "acct_3"]);
    let last_failure = state.last_failure.unwrap();
    assert_eq!(last_failure.account_id, "acct_3");
    assert_eq!(last_failure.message, "blocked by account 3");
}

#[tokio::test]
async fn redis_affinity_store_should_not_clear_recreated_cyber_policy_state_with_stale_revision() {
    let store = RedisSessionAffinityStore::new(create_test_redis("affinity-cyber-cas").await);
    let first = store
        .record_cyber_policy_failure(
            "session-hash",
            &CyberPolicyFailureSnapshot {
                account_id: "acct_1".to_string(),
                event: "error".to_string(),
                message: "first failure".to_string(),
                upstream_code: Some("cyber_policy".to_string()),
            },
            3,
            Duration::hours(1),
        )
        .await
        .unwrap();
    assert!(
        store
            .clear_cyber_policy_state("session-hash", &first.revision)
            .await
            .unwrap()
    );
    let newer = store
        .record_cyber_policy_failure(
            "session-hash",
            &CyberPolicyFailureSnapshot {
                account_id: "acct_2".to_string(),
                event: "error".to_string(),
                message: "newer failure".to_string(),
                upstream_code: Some("cyber_policy".to_string()),
            },
            3,
            Duration::hours(1),
        )
        .await
        .unwrap();

    assert!(
        !store
            .clear_cyber_policy_state("session-hash", &first.revision)
            .await
            .unwrap()
    );
    assert!(
        store
            .clear_cyber_policy_state("session-hash", &newer.revision)
            .await
            .unwrap()
    );
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
        created_at,
    }
}
