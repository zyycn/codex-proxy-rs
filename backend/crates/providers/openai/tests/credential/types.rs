use chrono::Utc;
use provider_openai::credential::{
    CodexAccountProfile, CodexCookie, CodexCredentialData, CodexCredentialPrincipal,
    CodexOAuthCredentialData, CodexOAuthSecret,
};
use secrecy::{ExposeSecret as _, SecretString};

#[test]
fn oauth_secret_debug_redacts_every_token() {
    let secret = CodexOAuthSecret {
        access_token: SecretString::from("access-private"),
        refresh_token: Some(SecretString::from("refresh-private")),
        id_token: Some(SecretString::from("id-private")),
        web_access_token: Some(SecretString::from("web-private")),
    };
    let debug = format!("{secret:?}");
    for value in [
        "access-private",
        "refresh-private",
        "id-private",
        "web-private",
    ] {
        assert!(!debug.contains(value));
    }
}

#[test]
fn account_profile_debug_redacts_identity_fields() {
    let profile = CodexAccountProfile {
        email: Some("private@example.com".to_owned()),
        oauth_subject: "subject-private".to_owned(),
        poid: Some("poid-private".to_owned()),
        chatgpt_account_id: "chatgpt-private".to_owned(),
        chatgpt_user_id: "user-private".to_owned(),
        plan_type: Some("pro".to_owned()),
        access_token_expires_at: Some(Utc::now()),
    };
    let debug = format!("{profile:?}");
    assert!(!debug.contains("private@example.com"));
    assert!(!debug.contains("chatgpt-private"));
    assert!(!debug.contains("user-private"));
    assert!(debug.contains("pro"));
}

#[test]
fn plaintext_provider_schema_round_trips_dynamic_cookie_data() {
    let data = CodexCredentialData::OAuth(CodexOAuthCredentialData {
        schema_version: 1,
        principal: Some(CodexCredentialPrincipal {
            oauth_subject: "subject-private".to_owned(),
            poid: Some("poid-private".to_owned()),
        }),
        installation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
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
        web_access_token: Some("web-token-private".to_owned()),
    });
    let encoded = serde_json::to_value(&data).expect("serialize provider JSON");
    let decoded: CodexCredentialData =
        serde_json::from_value(encoded).expect("deserialize provider JSON");
    assert_eq!(decoded.oauth().expect("OAuth data").schema_version, 1);
    assert_eq!(decoded.cookies()[0].name, "oai-did");
    assert!(decoded.has_web_token());
    assert_eq!(decoded.web_access_token(), Some("web-token-private"));
    assert!(!format!("{decoded:?}").contains("cookie-private"));
    assert!(!format!("{decoded:?}").contains("web-token-private"));
}

#[test]
fn provider_schema_rejects_unknown_public_layer_fields() {
    let value = serde_json::json!({
        "schema_version": 1,
        "principal": {"oauth_subject": "subject-private", "poid": null},
        "installation_id": "00000000-0000-4000-8000-000000000001",
        "access_token": "at",
        "cookies": [],
        "unknown_field": 9
    });
    assert!(serde_json::from_value::<CodexCredentialData>(value).is_err());
    assert!(
        serde_json::from_value::<CodexCredentialData>(serde_json::json!({
            "schema_version": 1,
            "access_token": "at",
            "cookies": []
        }))
        .is_err()
    );
}

#[test]
fn reset_credits_authorization_header_prefers_web_access_token() {
    use provider_openai::credential::CodexCredentialCodec;

    let credential = CodexCredentialCodec::encode_complete(CodexCredentialData::OAuth(
        CodexOAuthCredentialData {
            schema_version: 1,
            principal: None,
            installation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            access_token: "at-openai-pat".to_owned(),
            refresh_token: None,
            id_token: None,
            oauth_client_id: None,
            oauth_scope: None,
            cookies: vec![],
            web_access_token: Some("ey-web-access-token".to_owned()),
        },
    ))
    .expect("encode");
    let decoded = CodexCredentialCodec::decode(&credential).expect("decode");

    // Standard API calls use primary access token (at-...)
    assert_eq!(
        decoded
            .authentication
            .authorization_header()
            .unwrap()
            .expose_secret(),
        "Bearer at-openai-pat"
    );

    // Reset credits uses web access token (ey-...)
    assert_eq!(
        decoded
            .authentication
            .reset_credits_authorization_header()
            .unwrap()
            .expose_secret(),
        "Bearer ey-web-access-token"
    );
}

#[test]
fn reset_credits_authorization_header_falls_back_to_access_token_when_web_token_is_missing() {
    use provider_openai::credential::CodexCredentialCodec;

    let credential = CodexCredentialCodec::encode_complete(CodexCredentialData::OAuth(
        CodexOAuthCredentialData {
            schema_version: 1,
            principal: None,
            installation_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            access_token: "ey-standard-oauth".to_owned(),
            refresh_token: Some("rt-refresh".to_owned()),
            id_token: None,
            oauth_client_id: None,
            oauth_scope: None,
            cookies: vec![],
            web_access_token: None,
        },
    ))
    .expect("encode");
    let decoded = CodexCredentialCodec::decode(&credential).expect("decode");

    assert_eq!(
        decoded
            .authentication
            .authorization_header()
            .unwrap()
            .expose_secret(),
        "Bearer ey-standard-oauth"
    );
    assert_eq!(
        decoded
            .authentication
            .reset_credits_authorization_header()
            .unwrap()
            .expose_secret(),
        "Bearer ey-standard-oauth"
    );
}
