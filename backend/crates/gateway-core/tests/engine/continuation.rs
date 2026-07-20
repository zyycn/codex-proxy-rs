use gateway_core::engine::continuation::{
    ContinuationBinding, NativeClaim, NativeClaimState, NativeContinuationPin,
    NativeContinuationReuse, PreviousResponseId,
};
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::error::SafeUpstreamValue;
use gateway_core::routing::{ProviderInstanceId, ProviderKind};

fn pin() -> NativeContinuationPin {
    NativeContinuationPin::new(
        PreviousResponseId::new("response-private").expect("valid response"),
        SafeUpstreamValue::new("upstream-private").expect("valid upstream response"),
        ProviderKind::new("openai").expect("valid provider"),
        ProviderInstanceId::new("inst_openai").expect("valid instance"),
        ProviderAccountId::new("acct_codex").expect("valid account"),
        NativeContinuationReuse::SingleUse,
    )
}

#[test]
fn native_pin_debug_should_redact_previous_response_id() {
    let debug = format!("{:?}", pin());
    assert!(!debug.contains("response-private"));
    assert!(!debug.contains("upstream-private"));
}

#[test]
fn external_binding_debug_should_redact_previous_response_id() {
    let binding = ContinuationBinding::External(
        PreviousResponseId::new("external-private").expect("valid response"),
    );

    assert!(!format!("{binding:?}").contains("external-private"));
}

#[test]
fn native_pin_should_reject_different_account() {
    assert!(!pin().matches(
        &ProviderKind::new("openai").expect("valid provider"),
        &ProviderInstanceId::new("inst_openai").expect("valid instance"),
        &ProviderAccountId::new("acct_other").expect("valid account"),
    ));
}

#[test]
fn single_use_claim_should_not_be_claimed_twice() {
    let mut claim = NativeClaim::new(NativeContinuationReuse::SingleUse);
    claim.claim().expect("first claim succeeds");

    assert!(claim.claim().is_err());
}

#[test]
fn proven_not_sent_should_release_single_use_claim() {
    let mut claim = NativeClaim::new(NativeContinuationReuse::SingleUse);
    claim.claim().expect("claim succeeds");
    claim.release_not_sent();

    assert_eq!(claim.state(), NativeClaimState::Available);
}

#[test]
fn ambiguous_send_should_make_single_use_claim_terminal() {
    let mut claim = NativeClaim::new(NativeContinuationReuse::SingleUse);
    claim.claim().expect("claim succeeds");
    claim.mark_ambiguous();

    assert_eq!(claim.state(), NativeClaimState::Ambiguous);
}
