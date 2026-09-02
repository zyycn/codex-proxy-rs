use std::time::{Duration, Instant};

use gateway_core::account::{AccountAttemptFeedback, AccountFeedbackStats, ProviderAccountId};
use gateway_core::routing::ProviderKind;

const FAILURE_RATE_HALF_LIFE: Duration = Duration::from_secs(15 * 60);

fn feedback_subject() -> (AccountFeedbackStats, ProviderKind, ProviderAccountId) {
    (
        AccountFeedbackStats::default(),
        ProviderKind::new("openai").expect("valid provider"),
        ProviderAccountId::new("acct_decay").expect("valid account"),
    )
}

fn report_failure(
    feedback: &AccountFeedbackStats,
    provider: &ProviderKind,
    account: &ProviderAccountId,
    observed_at: Instant,
) {
    feedback.report_at(
        provider,
        account,
        AccountAttemptFeedback::Failed {
            first_output_ms: None,
        },
        observed_at,
    );
}

#[test]
fn account_failure_rate_should_halve_after_one_half_life() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    report_failure(&feedback, &provider, &account, observed_at);

    let failure_rate = feedback
        .scheduling_signals_at(&provider, &account, observed_at + FAILURE_RATE_HALF_LIFE)
        .0;

    assert_eq!(failure_rate, Some(1_000));
}

#[test]
fn account_failure_rate_should_quarter_after_two_half_lives() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    report_failure(&feedback, &provider, &account, observed_at);

    let failure_rate = feedback
        .scheduling_signals_at(
            &provider,
            &account,
            observed_at + FAILURE_RATE_HALF_LIFE * 2,
        )
        .0;

    assert_eq!(failure_rate, Some(500));
}

#[test]
fn account_failure_rate_should_decay_before_applying_a_new_sample() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    report_failure(&feedback, &provider, &account, observed_at);
    feedback.report_at(
        &provider,
        &account,
        AccountAttemptFeedback::Succeeded {
            first_output_ms: None,
        },
        observed_at + FAILURE_RATE_HALF_LIFE,
    );

    let failure_rate = feedback
        .scheduling_signals_at(&provider, &account, observed_at + FAILURE_RATE_HALF_LIFE)
        .0;

    assert_eq!(failure_rate, Some(800));
}

#[test]
fn concurrent_account_failures_should_not_lose_samples() {
    let (feedback, provider, account) = feedback_subject();
    let observed_at = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| report_failure(&feedback, &provider, &account, observed_at));
        }
    });

    let failure_rate = feedback
        .scheduling_signals_at(&provider, &account, observed_at)
        .0;

    assert_eq!(failure_rate, Some(8_322));
}
