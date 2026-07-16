use codex_proxy_rs::fleet::scheduler::{AttemptFeedback, FeedbackSample, FeedbackStats};

#[test]
fn sample_of_unknown_account_is_empty() {
    let stats = FeedbackStats::new();
    assert_eq!(stats.sample("missing"), FeedbackSample::default());
}

#[test]
fn failed_before_first_token_is_recorded_as_a_full_pre_token_failure() {
    let stats = FeedbackStats::new();
    stats.report_attempt("acct", AttemptFeedback::FailedBeforeFirstToken);
    let sample = stats.sample("acct");
    assert_eq!(sample.pre_token_failure_rate, Some(1.0));
    assert_eq!(sample.ttft_ms, None);
}

#[test]
fn successful_first_token_decays_pre_token_failure_and_updates_ttft() {
    let stats = FeedbackStats::new();
    stats.report_attempt("acct", AttemptFeedback::FailedBeforeFirstToken);
    stats.report_attempt(
        "acct",
        AttemptFeedback::Completed {
            first_token_ms: Some(1000),
        },
    );
    let sample = stats.sample("acct");
    assert!((sample.pre_token_failure_rate.unwrap() - 0.8).abs() < 1e-9);
    assert_eq!(sample.ttft_ms, Some(1000.0));
}

#[test]
fn failed_after_first_token_tracks_abort_without_penalizing_pre_token_failure() {
    let stats = FeedbackStats::new();
    stats.report_attempt(
        "acct",
        AttemptFeedback::FailedAfterFirstToken {
            first_token_ms: 200,
        },
    );
    let sample = stats.sample("acct");
    assert_eq!(sample.pre_token_failure_rate, Some(0.0));
    assert_eq!(sample.post_token_abort_rate, Some(1.0));
    assert_eq!(sample.ttft_ms, Some(200.0));
}

#[test]
fn remove_clears_account() {
    let stats = FeedbackStats::new();
    stats.report_attempt("acct", AttemptFeedback::FailedBeforeFirstToken);
    stats.remove("acct");
    assert_eq!(stats.sample("acct"), FeedbackSample::default());
}
