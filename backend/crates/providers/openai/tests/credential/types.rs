use chrono::Utc;
use provider_openai::credential::{
    CodexAccountProfile, CodexCookie, CodexCredentialData, CodexOAuthSecret,
};
use secrecy::SecretString;

#[test]
fn oauth_secret_debug_redacts_every_token() {
    let secret = CodexOAuthSecret {
        access_token: SecretString::from("access-private"),
        refresh_token: Some(SecretString::from("refresh-private")),
        id_token: Some(SecretString::from("id-private")),
    };
    let debug = format!("{secret:?}");
    for value in ["access-private", "refresh-private", "id-private"] {
        assert!(!debug.contains(value));
    }
}

#[test]
fn account_profile_debug_redacts_identity_fields() {
    let profile = CodexAccountProfile {
        email: Some("private@example.com".to_owned()),
        chatgpt_account_id: "chatgpt-private".to_owned(),
        chatgpt_user_id: Some("user-private".to_owned()),
        plan_type: Some("pro".to_owned()),
        access_token_expires_at: Some(Utc::now()),
        next_refresh_at: None,
    };
    let debug = format!("{profile:?}");
    assert!(!debug.contains("private@example.com"));
    assert!(!debug.contains("chatgpt-private"));
    assert!(!debug.contains("user-private"));
    assert!(debug.contains("pro"));
}

#[test]
fn plaintext_provider_schema_round_trips_dynamic_cookie_data() {
    let data = CodexCredentialData {
        schema_version: 1,
        access_token: "at".to_owned(),
        refresh_token: Some("rt".to_owned()),
        id_token: None,
        oauth_client_id: Some("client".to_owned()),
        oauth_scope: Some("openid profile".to_owned()),
        cookies: vec![CodexCookie {
            name: "oai-did".to_owned(),
            value: "cookie-private".to_owned(),
            domain: "chatgpt.com".to_owned(),
            path: "/".to_owned(),
            host_only: false,
            secure: true,
            expires_at: None,
        }],
    };
    let encoded = serde_json::to_value(&data).expect("serialize provider JSON");
    let decoded: CodexCredentialData =
        serde_json::from_value(encoded).expect("deserialize provider JSON");
    assert_eq!(decoded.schema_version, 1);
    assert_eq!(decoded.cookies[0].name, "oai-did");
    assert!(!format!("{decoded:?}").contains("cookie-private"));
}

#[test]
fn provider_schema_rejects_unknown_public_layer_fields() {
    let value = serde_json::json!({
        "schema_version": 1,
        "access_token": "at",
        "cookies": [],
        "unknown_field": 9
    });
    assert!(serde_json::from_value::<CodexCredentialData>(value).is_err());
}
