use chrono::Utc;
use codex_proxy_rs::upstream::accounts::model::AccountStatus;
use codex_proxy_rs::upstream::scheduler::{rank_candidates, FeedbackStats, ScoreWeights};

use crate::support::accounts::test_account;

#[test]
fn ranks_idle_account_above_loaded_one() {
    let mut loaded = test_account("loaded", AccountStatus::Active);
    loaded.window_input_tokens = 1_000_000;
    let idle = test_account("idle", AccountStatus::Active);
    let candidates = vec![loaded, idle];
    let feedback = FeedbackStats::new();
    let slot_count = |_: &str| 0;

    let ranked = rank_candidates(
        &candidates,
        &ScoreWeights::default(),
        Utc::now().timestamp(),
        &slot_count,
        &feedback,
    );
    // idle 账号（索引 1）窗口用量为 0，应排在负载重的账号之前。
    assert_eq!(ranked.first().unwrap().0, 1);
}

#[test]
fn high_error_rate_account_is_penalized() {
    let healthy = test_account("healthy", AccountStatus::Active);
    let flaky = test_account("flaky", AccountStatus::Active);
    let candidates = vec![healthy, flaky];
    let feedback = FeedbackStats::new();
    // flaky 连续失败，错误率 EWMA 拉高。
    for _ in 0..5 {
        feedback.report("flaky", false, None);
    }
    let slot_count = |_: &str| 0;

    let ranked = rank_candidates(
        &candidates,
        &ScoreWeights::default(),
        Utc::now().timestamp(),
        &slot_count,
        &feedback,
    );
    // healthy（索引 0）应排在 flaky 之前。
    assert_eq!(ranked.first().unwrap().0, 0);
}

#[test]
fn quota_limited_account_ranks_last_despite_being_idle() {
    let mut limited = test_account("limited", AccountStatus::Active);
    limited.quota_limit_reached = true;
    let mut busy = test_account("busy", AccountStatus::Active);
    busy.window_input_tokens = 1_000_000;
    busy.window_request_count = 100;
    let candidates = vec![limited, busy];
    let feedback = FeedbackStats::new();
    let slot_count = |_: &str| 0;

    let ranked = rank_candidates(
        &candidates,
        &ScoreWeights::default(),
        Utc::now().timestamp(),
        &slot_count,
        &feedback,
    );
    // 即使 limited 完全空闲，配额封顶惩罚也应让繁忙但未封顶的 busy（索引 1）排前。
    assert_eq!(ranked.first().unwrap().0, 1);
}

#[test]
fn actual_token_load_dominates_cached() {
    // cache_heavy 实际 token 更少（但缓存高），heavier_actual 实际 token 更多。
    // 实际用量权重远大于缓存，cache_heavy（索引 0）应排前。
    let mut cache_heavy = test_account("cache_heavy", AccountStatus::Active);
    cache_heavy.window_input_tokens = 10_000;
    cache_heavy.window_cached_tokens = 100_000;
    let mut heavier_actual = test_account("heavier_actual", AccountStatus::Active);
    heavier_actual.window_input_tokens = 20_000;
    let candidates = vec![cache_heavy, heavier_actual];
    let feedback = FeedbackStats::new();
    let slot_count = |_: &str| 0;

    let ranked = rank_candidates(
        &candidates,
        &ScoreWeights::default(),
        Utc::now().timestamp(),
        &slot_count,
        &feedback,
    );
    assert_eq!(ranked.first().unwrap().0, 0);
}

#[test]
fn cached_breaks_ties_when_actual_load_matches() {
    // 实际 token 相同，缓存少者（cache_light，索引 1）应排前。
    let mut cache_heavy = test_account("cache_heavy", AccountStatus::Active);
    cache_heavy.window_input_tokens = 10_000;
    cache_heavy.window_cached_tokens = 100_000;
    let mut cache_light = test_account("cache_light", AccountStatus::Active);
    cache_light.window_input_tokens = 10_000;
    cache_light.window_cached_tokens = 1_000;
    let candidates = vec![cache_heavy, cache_light];
    let feedback = FeedbackStats::new();
    let slot_count = |_: &str| 0;

    let ranked = rank_candidates(
        &candidates,
        &ScoreWeights::default(),
        Utc::now().timestamp(),
        &slot_count,
        &feedback,
    );
    assert_eq!(ranked.first().unwrap().0, 1);
}

#[test]
fn zero_weights_produce_flat_scores() {
    let candidates = vec![
        test_account("a", AccountStatus::Active),
        test_account("b", AccountStatus::Active),
    ];
    let feedback = FeedbackStats::new();
    let weights = ScoreWeights {
        load: 0.0,
        window: 0.0,
        cached: 0.0,
        window_requests: 0.0,
        error_rate: 0.0,
        ttft: 0.0,
        reset: 0.0,
    };
    let slot_count = |_: &str| 0;

    let ranked = rank_candidates(
        &candidates,
        &weights,
        Utc::now().timestamp(),
        &slot_count,
        &feedback,
    );
    assert!(ranked.iter().all(|(_, b)| b.total == 0.0));
}
