use provider_xai::{
    FailureClass, OAuthHttpResponse, OAuthOperation, OAuthPrincipal, parse_oauth_error,
    parse_refresh_success,
};

#[test]
fn token_debug_should_not_expose_fixture_secrets() {
    let response = OAuthHttpResponse::new(
        200,
        include_bytes!("fixtures/refresh_success.json").to_vec(),
    );
    let tokens = parse_refresh_success(&response).expect("fixture token response is valid");

    let debug = format!("{tokens:?}");

    assert!(
        !debug.contains("fixture-access-token"),
        "debug output was {debug}"
    );
}

#[test]
fn invalid_grant_should_be_classified_as_permanent_credential_failure() {
    let response =
        OAuthHttpResponse::new(400, include_bytes!("fixtures/invalid_grant.json").to_vec());
    let error = parse_oauth_error(&response, OAuthOperation::RefreshToken);

    assert_eq!(error.class(), FailureClass::CredentialPermanent);
}

#[test]
fn principal_debug_should_redact_identity() {
    let principal =
        OAuthPrincipal::new("team", "sensitive-team-id").expect("fixture principal is valid");

    let debug = format!("{principal:?}");

    assert!(
        !debug.contains("sensitive-team-id"),
        "debug output was {debug}"
    );
}
