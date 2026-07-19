use url::Url;

use provider_xai::{FormField, HttpHeader, OAuthHttpRequest, SecretValue};

#[test]
fn request_debug_should_redact_sensitive_form_values() {
    let request = OAuthHttpRequest::post(
        Url::parse("https://auth.x.ai/oauth2/token").expect("fixture URL is valid"),
        vec![HttpHeader::new("x-grok-client-version", "test")],
        vec![FormField::secret(
            "refresh_token",
            SecretValue::new("refresh-secret".to_owned()),
        )],
    );

    let debug = format!("{request:?}");
    assert!(
        !debug.contains("refresh-secret"),
        "debug output was {debug}"
    );
}
