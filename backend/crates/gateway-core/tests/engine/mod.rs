mod admission;
mod continuation;
mod coordinator;
mod credential;
mod execution;
mod probe;
mod provider;

use gateway_core::engine::{AttemptTrigger, CancellationToken, UpstreamSendState};

#[test]
fn upstream_send_state_should_keep_ambiguous_distinct_from_sent() {
    assert_ne!(UpstreamSendState::Ambiguous, UpstreamSendState::Sent);
}

#[test]
fn attempt_trigger_names_should_match_ops_event_contract() {
    assert_eq!(AttemptTrigger::AccountRetry.as_str(), "account_retry");
}

#[test]
fn cancellation_token_should_wake_current_state() {
    let token = CancellationToken::new();
    token.cancel();

    assert!(token.is_cancelled());
}
