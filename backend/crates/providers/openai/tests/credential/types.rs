//! 验证 OpenAI 凭据类型的脱敏、Cookie 往返与字段约束

use chrono::Utc;
use provider_openai::credential::{
    CodexAccountProfile, CodexCookie, CodexCredentialData, CodexCredentialPrincipal,
    CodexOAuthCredentialData, CodexOAuthSecret,
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
        transport: provider_openai::credential::ResponsesTransport::PreferWebsocket,
        websocket_max_age_ms: None,
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
    });
    let encoded = serde_json::to_value(&data).expect("serialize provider JSON");
    let decoded: CodexCredentialData =
        serde_json::from_value(encoded).expect("deserialize provider JSON");
    assert_eq!(decoded.oauth().expect("OAuth data").schema_version, 1);
    assert_eq!(decoded.cookies()[0].name, "oai-did");
    assert!(!format!("{decoded:?}").contains("cookie-private"));
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
fn provider_schema_accepts_account_websocket_max_age_override() {
    let value = serde_json::json!({
        "schema_version": 1,
        "installation_id": "00000000-0000-4000-8000-000000000001",
        "access_token": "test-access-token",
        "cookies": [],
        "websocket_max_age_ms": 180_000
    });
    let data = serde_json::from_value::<CodexCredentialData>(value)
        .expect("an account may configure a positive WebSocket reuse age");
    assert_eq!(
        serde_json::to_value(data).unwrap()["websocket_max_age_ms"],
        180_000
    );
}

#[test]
fn provider_schema_rejects_zero_websocket_max_age_override() {
    let value = serde_json::json!({
        "schema_version": 1,
        "installation_id": "00000000-0000-4000-8000-000000000001",
        "access_token": "test-access-token",
        "cookies": [],
        "websocket_max_age_ms": 0
    });
    assert!(serde_json::from_value::<CodexCredentialData>(value).is_err());
}

#[test]
fn provider_schema_websocket_max_age_is_optional_and_positive_for_both_authentication_kinds() {
    for value in [
        serde_json::json!({
            "schema_version": 1,
            "installation_id": "00000000-0000-4000-8000-000000000001",
            "access_token": "synthetic-access-token",
            "cookies": []
        }),
        serde_json::json!({
            "schema_version": 1,
            "installation_id": "00000000-0000-4000-8000-000000000001",
            "base_url": "https://example.invalid/v1",
            "api_key": "synthetic-api-key"
        }),
    ] {
        let data: CodexCredentialData = serde_json::from_value(value.clone()).unwrap();
        assert!(
            serde_json::to_value(data)
                .unwrap()
                .get("websocket_max_age_ms")
                .is_none()
        );
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("30000"),
            serde_json::json!(true),
        ] {
            let mut invalid_value = value.clone();
            invalid_value["websocket_max_age_ms"] = invalid;
            assert!(serde_json::from_value::<CodexCredentialData>(invalid_value).is_err());
        }
        let mut configured = value;
        configured["websocket_max_age_ms"] = serde_json::json!(30_001);
        let data: CodexCredentialData = serde_json::from_value(configured).unwrap();
        assert_eq!(
            serde_json::to_value(data).unwrap()["websocket_max_age_ms"],
            30_001
        );
    }
}
