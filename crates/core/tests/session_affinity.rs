use chrono::{Duration, TimeZone, Utc};
use codex_proxy_core::serving::affinity::{
    SessionAffinityEntry, SessionAffinityMap, StoredSessionAffinity,
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
