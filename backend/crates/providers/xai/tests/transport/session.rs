use provider_xai::{GrokSessionBinding, GrokSessionDataError};

#[test]
fn session_binding_should_reject_reserved_values() {
    let result = GrokSessionBinding::new("__reserved");

    assert_eq!(result, Err(GrokSessionDataError::InvalidBinding));
}

#[test]
fn session_binding_debug_should_hide_pseudonym() {
    let binding = GrokSessionBinding::new("binding-reference").expect("valid binding");

    assert_eq!(format!("{binding:?}"), "GrokSessionBinding([PSEUDONYM])");
}
