mod admission;
mod connection;
mod continuation;
mod coordinator;
mod execution;
mod extensions;
mod middleware;
mod policy;
mod probe;
mod provider;
mod response_control;

use gateway_core::engine::AttemptTrigger;

#[test]
fn attempt_trigger_names_should_match_ops_event_contract() {
    assert_eq!(AttemptTrigger::AccountRetry.as_str(), "account_retry");
}
