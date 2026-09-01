use gateway_core::account::ProviderAccountId;
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::ProviderKind;

fn pin() -> NativeContinuationPin {
    NativeContinuationPin::new(
        PreviousResponseId::new("response-private"),
        PreviousResponseId::new("upstream-private"),
        ClientApiKeyId::new("key_client").expect("valid client key"),
        ProviderKind::new("openai").expect("valid provider"),
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
    let binding = ContinuationBinding::External(PreviousResponseId::new("external-private"));

    assert!(!format!("{binding:?}").contains("external-private"));
}

#[test]
fn opaque_response_id_preserves_any_codex_string_without_debug_disclosure() {
    let value = format!("resp_{}\0opaque", "x".repeat(257));
    let response_id = PreviousResponseId::new(value.clone());

    assert_eq!(response_id.as_str(), value);
    assert!(!format!("{response_id:?}").contains(&value));
}

#[test]
fn native_pin_should_reject_different_account() {
    assert!(!pin().matches(
        &ProviderKind::new("openai").expect("valid provider"),
        &ProviderAccountId::new("acct_other").expect("valid account"),
    ));
}

#[test]
fn native_pin_should_reject_different_client_api_key() {
    assert!(!pin().matches_client(&ClientApiKeyId::new("key_other").expect("valid client key")));
}
