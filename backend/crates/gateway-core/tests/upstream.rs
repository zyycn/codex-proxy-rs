use gateway_core::upstream::UpstreamSendState;

#[test]
fn upstream_send_state_should_keep_ambiguous_distinct_from_sent() {
    assert_ne!(UpstreamSendState::Ambiguous, UpstreamSendState::Sent);
}
