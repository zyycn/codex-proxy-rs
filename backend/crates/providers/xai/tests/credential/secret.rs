use provider_xai::SecretValue;

#[test]
fn debug_should_not_expose_secret_value() {
    let secret = SecretValue::new("top-secret-token".to_owned());

    assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");
}

#[test]
fn constant_time_eq_should_reject_different_values() {
    let left = SecretValue::new("state-one".to_owned());
    let right = SecretValue::new("state-two".to_owned());

    assert!(!left.constant_time_eq(&right));
}
