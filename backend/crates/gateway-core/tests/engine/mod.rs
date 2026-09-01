mod admission;
mod continuation;
mod coordinator;
mod execution;
mod probe;
mod provider;

use gateway_core::engine::AttemptTrigger;

#[test]
fn attempt_trigger_names_should_match_ops_event_contract() {
    assert_eq!(AttemptTrigger::AccountRetry.as_str(), "account_retry");
}
