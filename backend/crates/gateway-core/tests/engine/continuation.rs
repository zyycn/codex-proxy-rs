use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
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
