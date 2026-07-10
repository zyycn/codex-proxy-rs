use codex_proxy_rs::accounts::scheduler::{FeedbackSample, FeedbackStats};

#[test]
fn sample_of_unknown_account_is_empty() {
    let stats = FeedbackStats::new();
    assert_eq!(stats.sample("missing"), FeedbackSample::default());
}

#[test]
fn first_sample_lands_directly() {
    let stats = FeedbackStats::new();
    stats.report("acct", false, Some(400));
    let sample = stats.sample("acct");
    assert_eq!(sample.error_rate, Some(1.0));
    assert_eq!(sample.ttft_ms, Some(400.0));
}

#[test]
fn subsequent_samples_blend_by_alpha() {
    let stats = FeedbackStats::new();
    stats.report("acct", false, Some(1000)); // error_rate=1.0, ttft=1000
    stats.report("acct", true, Some(0)); // error_rate=0.8, ttft=800
    let sample = stats.sample("acct");
    assert!((sample.error_rate.unwrap() - 0.8).abs() < 1e-9);
    assert!((sample.ttft_ms.unwrap() - 800.0).abs() < 1e-9);
}

#[test]
fn success_only_reports_decay_error_rate() {
    let stats = FeedbackStats::new();
    stats.report("acct", false, None); // error_rate=1.0, ttft untouched
    stats.report("acct", true, None); // error_rate=0.8
    let sample = stats.sample("acct");
    assert!((sample.error_rate.unwrap() - 0.8).abs() < 1e-9);
    assert_eq!(sample.ttft_ms, None);
}

#[test]
fn remove_clears_account() {
    let stats = FeedbackStats::new();
    stats.report("acct", false, Some(100));
    stats.remove("acct");
    assert_eq!(stats.sample("acct"), FeedbackSample::default());
}
