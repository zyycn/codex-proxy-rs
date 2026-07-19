use chrono::{TimeZone, Utc};
use gateway_api::admin::xai::{
    AuthorizationStartData, CompleteAuthorizationRequest, NewCredentialRequest,
    StartAuthorizationRequest, XaiCredentialImportData, XaiCredentialImportDocumentRequest,
    XaiCredentialListData, XaiCredentialViewData,
};
use serde_json::{Value, json};

fn valid_request() -> Value {
    json!({
        "expectedConfigRevision": 7,
        "providerInstanceId": "inst_xai",
        "document": {
            "type": "oauth-account-bundle",
            "version": 1,
            "exported_at": "2026-07-18T02:47:01Z",
            "proxies": [],
            "accounts": [{
                "name": "Grok OAuth",
                "platform": "grok",
                "type": "oauth",
                "credentials": {
                    "access_token": "access-token-secret",
                    "refresh_token": "refresh-token-secret",
                    "token_type": "Bearer",
                    "expires_at": "2026-07-18T06:47:01Z",
                    "email": "ignored@example.com",
                    "base_url": "https://cli-chat-proxy.grok.com/v1",
                    "id_token": "header.payload.signature",
                    "client_id": "official-client",
                    "scope": "openid offline_access grok-cli:access api:access"
                },
                "extra": {"email": "also-ignored@example.com"},
                "concurrency": 1,
                "priority": 0
            }]
        }
    })
}

#[test]
fn import_document_accepts_opaque_provider_owned_shape() {
    let request: XaiCredentialImportDocumentRequest =
        serde_json::from_value(valid_request()).expect("deserialize Provider document");

    request
        .validate()
        .expect("validate Provider document boundary");
}

#[test]
fn import_document_keeps_inner_schema_owned_by_provider() {
    let mut request = valid_request();
    request["document"]["accounts"][0]["credentials"]["provider_future_field"] =
        json!({"nested": true});

    let request = serde_json::from_value::<XaiCredentialImportDocumentRequest>(request)
        .expect("API must not decode Provider fields");

    assert_eq!(request.validate(), Ok(()));
}

#[test]
fn import_document_rejects_unknown_outer_field() {
    let mut request = valid_request();
    request["schemaVersion"] = json!(1);
    let error = serde_json::from_value::<XaiCredentialImportDocumentRequest>(request)
        .err()
        .expect("unknown API field must fail");

    assert!(error.to_string().contains("unknown field `schemaVersion`"));
}

#[test]
fn import_document_rejects_non_object_document() {
    let mut request = valid_request();
    request["document"] = json!([]);
    let request: XaiCredentialImportDocumentRequest =
        serde_json::from_value(request).expect("deserialize generic JSON");

    assert_eq!(request.validate().unwrap_err().field(), "document");
}

#[test]
fn import_response_should_never_contain_identity_or_secret_fields() {
    let response = XaiCredentialImportData::new(8, vec!["cred_xai_safe".to_owned()]);
    let rendered = serde_json::to_string(&response).expect("serialize response");

    assert_eq!(
        rendered,
        r#"{"configRevision":8,"importedCount":1,"credentialIds":["cred_xai_safe"]}"#
    );
}

#[test]
fn credential_list_wire_should_expose_only_safe_view_fields() {
    let timestamp = Utc
        .with_ymd_and_hms(2026, 7, 18, 3, 0, 0)
        .single()
        .expect("valid fixture timestamp");
    let response = XaiCredentialListData {
        config_revision: 8,
        items: vec![XaiCredentialViewData {
            id: "cred_xai_safe".to_owned(),
            provider_instance_id: "inst_xai".to_owned(),
            name: "Grok OAuth".to_owned(),
            email: Some("verified@example.com".to_owned()),
            upstream_user_id: "subject_xai".to_owned(),
            upstream_account_id: None,
            plan_type: Some("pro".to_owned()),
            enabled: true,
            credential_revision: 2,
            has_refresh_token: true,
            availability: "ready".to_owned(),
            availability_reason: None,
            access_token_expires_at: timestamp,
            next_refresh_at: Some(timestamp),
            cooldown_until: None,
            created_at: timestamp,
            updated_at: timestamp,
        }],
    };
    let rendered = serde_json::to_string(&response).expect("serialize typed list response");

    assert!(rendered.contains("credentialRevision"));
    assert!(rendered.contains("accessTokenExpiresAt"));
    for forbidden in [
        "\"accessToken\":",
        "\"refreshToken\":",
        "\"idToken\":",
        "\"subject\":",
    ] {
        assert!(!rendered.contains(forbidden), "wire leaked {forbidden}");
    }
}

#[test]
fn authorization_start_wire_contains_only_browser_flow_fields() {
    let response = AuthorizationStartData {
        flow_id: "flow_xai_safe".to_owned(),
        authorization_url: "https://auth.x.ai/oauth2/auth?state=redacted".to_owned(),
        expires_at: Utc::now(),
    };
    let rendered = serde_json::to_string(&response).expect("serialize authorization response");

    assert!(rendered.contains("authorizationUrl"));
    assert!(!rendered.contains("codeVerifier"));
    assert!(!rendered.contains("nonce"));
}

#[test]
fn authorization_requests_require_server_owned_flow_and_full_callback_url() {
    let start = StartAuthorizationRequest {
        credential: NewCredentialRequest {
            expected_config_revision: 7,
            provider_instance_id: "inst_xai".to_owned(),
            name: "xAI OAuth".to_owned(),
        },
    };
    start.validate().expect("authorization start is valid");

    let complete: CompleteAuthorizationRequest = serde_json::from_value(json!({
        "flowId": "flow_xai_safe",
        "callbackUrl": "http://127.0.0.1:56121/callback?code=redacted&state=redacted"
    }))
    .expect("deserialize callback completion");
    complete.validate().expect("callback completion is valid");
}
