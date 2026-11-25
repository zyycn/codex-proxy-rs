//! 导入使用真实 OAuth 交换边界，导出可重导入且凭据轮换保持租约与主体约束

use super::{
    CredentialService,
    oauth::{GoogleOAuth, OAuthRegistration},
};
use crate::support::account_record as account;
use crate::support::{Fixture, MemoryPorts};
use chrono::{Duration, Utc};
use gateway_admin::{
    model::{
        Revision,
        accounts::AccountRecord,
        provider_credentials::{
            PrepareCredentialImport, PrepareCredentialRotation, ProviderDocument,
            ProviderExportCredentialInput,
        },
    },
    ports::provider::ProviderAdminErrorKind as Kind,
};
use gateway_core::{
    account::{CredentialState, OpaqueProviderData, QuotaState},
    routing::ProviderKind,
};
use serde_json::{Value, json};
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

async fn fixture() -> (MockServer, CredentialService, Arc<MemoryPorts>) {
    let server = MockServer::start().await;
    let origin = server.uri();
    let config: OAuthRegistration = serde_json::from_value(json!({
        "client_id":"fixture-client", "client_secret":"fixture-secret",
        "redirect_uri":"http://127.0.0.1:8020/callback",
        "resource_scopes":["fixture-resource"], "resource_origin":origin,
    }))
    .unwrap();
    let store = Fixture::new(&origin).store;
    let credentials = CredentialService {
        kind: ProviderKind::new("antigravity").unwrap(),
        accounts: store.clone(),
        leases: store.clone(),
        runtime_policy: store.clone(),
        credential_state: store.clone(),
        release: Arc::new(
            crate::profile::ReleaseService::new(crate::support::profile::cache()).unwrap(),
        ),
        oauth: Arc::new(GoogleOAuth::with_endpoints(
            config,
            &format!("{origin}/token"),
            &format!("{origin}/userinfo"),
            &format!("{origin}/tokeninfo"),
        )),
    };
    (server, credentials, store)
}

fn document(value: Value) -> ProviderDocument {
    ProviderDocument::new(OpaqueProviderData::new(value.as_object().unwrap().clone()))
}

fn import(document: ProviderDocument) -> PrepareCredentialImport {
    PrepareCredentialImport {
        default_outbound_proxy: None,
        document,
    }
}

fn file() -> Value {
    json!({"provider":"antigravity","version":1,"accounts":[{"label":"Local name","refreshToken":"fixture-refresh"}]})
}

async fn token(server: &MockServer, response: Value) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("client_id=fixture-client"))
        .and(body_string_contains("client_secret=fixture-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(server)
        .await;
}

async fn identity(server: &MockServer, subject: &str) {
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("authorization", "Bearer fresh-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":subject, "email":"verified@example.org", "verified_email":true,
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn token_response() -> Value {
    json!({"access_token":"fresh-access","token_type":"Bearer","expires_in":3600,"scope":"email fixture-resource"})
}

#[tokio::test]
async fn expired_access_exports_as_refreshable_file_and_reimports_with_local_name() {
    let (server, credentials, _) = fixture().await;
    let mut record = account();
    record.access_token_expires_at = Some(Utc::now() - Duration::days(1));
    record.email = Some("previous@example.org".into());
    record.plan_type = Some("Google AI Pro".into());
    let material = document(json!({
        "registration":credentials.oauth.registration(), "subject":"fixture-subject",
        "access_token":"expired-access", "refresh_token":"fixture-refresh", "scopes":["untrusted-scope"],
    }));
    let exported = credentials
        .export_credentials(vec![ProviderExportCredentialInput {
            account: record.clone(),
            provider_material: material,
        }])
        .unwrap();
    assert!(!format!("{exported:?}").contains("fixture-refresh"));
    assert!(!format!("{exported:?}").contains("expired-access"));
    let mut exported_file = Value::Object(exported.document.into_provider_data().into_inner());
    let entry = &exported_file["accounts"][0];
    assert_eq!(entry["label"], record.name);
    assert_eq!(entry["accessToken"], "expired-access");
    assert_eq!(entry["refreshToken"], "fixture-refresh");
    assert_eq!(
        entry["accessTokenExpiresAt"],
        json!(record.access_token_expires_at)
    );
    assert_eq!(entry["email"], "previous@example.org");
    assert_eq!(entry["userId"], "fixture-subject");
    assert_eq!(entry["planType"], "Google AI Pro");
    assert_eq!(entry["scopes"], json!(["untrusted-scope"]));
    assert_eq!(entry["createdAt"], json!(record.created_at));
    assert_eq!(entry["updatedAt"], json!(record.updated_at));
    // 文件中的历史观测不能替代本次导入的身份、权限和有效期验证
    exported_file["accounts"][0]["userId"] = json!("forged-subject");
    exported_file["accounts"][0]["accessTokenExpiresAt"] = json!(Utc::now() + Duration::days(90));
    token(&server, token_response()).await;
    identity(&server, "fixture-subject").await;
    let result = credentials
        .prepare_import(import(document(exported_file)))
        .await
        .unwrap();
    let loaded = &result.credentials[0];
    assert_eq!(loaded.name, "Local name");
    assert_eq!(loaded.email.as_deref(), Some("verified@example.org"));
    assert_eq!(loaded.upstream_user_id.as_deref(), Some("fixture-subject"));
    assert!(loaded.has_refresh_token && loaded.access_token_expires_at.unwrap() > Utc::now());
    assert!(loaded.access_token_expires_at.unwrap() < Utc::now() + Duration::hours(2));
    assert_eq!(loaded.plan_type, None);
    let stored = loaded
        .provider_material
        .clone()
        .into_provider_data()
        .into_inner();
    assert_eq!(stored["access_token"], "fresh-access");
    assert_eq!(stored["refresh_token"], "fixture-refresh");
    assert_eq!(stored["scopes"], json!(["email", "fixture-resource"]));
}

#[tokio::test]
async fn unnamed_account_exports_and_reimports_with_verified_identity() {
    let (server, credentials, _) = fixture().await;
    let mut unnamed = account();
    unnamed.name.clear();
    let exported = credentials
        .export_credentials(vec![ProviderExportCredentialInput {
            account: unnamed,
            provider_material: document(json!({
                "registration":credentials.oauth.registration(), "subject":"fixture-subject",
                "access_token":"expired-access", "refresh_token":"fixture-refresh", "scopes":[],
            })),
        }])
        .unwrap();
    assert!(
        exported.document.expose_to_provider().expose_to_provider()["accounts"][0]
            .get("label")
            .is_none()
    );
    token(&server, token_response()).await;
    identity(&server, "fixture-subject").await;
    let imported = credentials
        .prepare_import(import(exported.document))
        .await
        .unwrap();
    assert_eq!(imported.credentials[0].name, "");
    assert_eq!(
        imported.credentials[0].email.as_deref(),
        Some("verified@example.org")
    );
    assert!(imported.credentials[0].has_refresh_token);
}

#[tokio::test]
async fn malformed_files_fail_before_any_refresh() {
    let (server, credentials, _) = fixture().await;
    let mut inputs = vec![];
    for (key, value) in [
        ("provider", json!("openai")),
        ("version", json!(2)),
        ("accounts", json!([])),
    ] {
        let mut input = file();
        input[key] = value;
        inputs.push(input);
    }
    let mut unknown = file();
    unknown["accounts"][0]["unknownField"] = json!(true);
    inputs.push(unknown);
    for (key, value) in [
        ("accessTokenExpiresAt", json!("not-a-timestamp")),
        ("scopes", json!("not-an-array")),
        ("userId", json!({})),
    ] {
        let mut malformed = file();
        malformed["accounts"][0][key] = value;
        inputs.push(malformed);
    }
    let mut duplicate = file();
    duplicate["accounts"]
        .as_array_mut()
        .unwrap()
        .push(file()["accounts"][0].clone());
    inputs.push(duplicate);
    let mut missing_offline = file();
    missing_offline["accounts"][0]["refreshToken"] = json!("");
    inputs.push(missing_offline);
    for input in inputs {
        assert_eq!(
            credentials
                .prepare_import(import(document(input)))
                .await
                .unwrap_err()
                .kind(),
            Kind::Invalid
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn imported_grants_cannot_inherit_missing_or_reduced_scopes() {
    let (server, credentials, _) = fixture().await;
    let mut input = file();
    input["accounts"][0]["scopes"] = json!(["email", "fixture-resource"]);
    for scope in [Value::Null, json!("openid email")] {
        server.reset().await;
        let mut response = token_response();
        response["scope"] = scope;
        token(&server, response).await;
        Mock::given(method("GET"))
            .and(path("/tokeninfo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"scope":"email"})))
            .mount(&server)
            .await;
        assert_eq!(
            credentials
                .prepare_import(import(document(input.clone())))
                .await
                .unwrap_err()
                .kind(),
            Kind::Invalid
        );
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.url.path() != "/userinfo")
        );
    }
}

#[tokio::test]
async fn import_resolves_scopes_and_freezes_global_profile_until_the_next_refresh() {
    let (server, credentials, store) = fixture().await;
    *store.request_profile.lock().unwrap() = Some(crate::profile::object(json!({
        "client":"cli", "platform":"windows", "versionMode":"fixed", "clientVersion":"1.5.15",
    })));
    let mut response = token_response();
    response.as_object_mut().unwrap().remove("scope");
    let policy = store.clone();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |_: &wiremock::Request| {
            *policy.request_profile.lock().unwrap() = Some(crate::profile::object(json!({
                "client":"desktop", "platform":"linux", "versionMode":"fixed", "clientVersion":"9.8.7",
            })));
            ResponseTemplate::new(200).set_body_json(response.clone())
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/tokeninfo"))
        .and(query_param("access_token", "fresh-access"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"scope":"email fixture-resource"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    identity(&server, "fixture-subject").await;
    let imported = credentials
        .prepare_import(import(document(file())))
        .await
        .unwrap();
    let stored = imported.credentials[0]
        .provider_material
        .clone()
        .into_provider_data()
        .into_inner();
    assert_eq!(stored["scopes"], json!(["email", "fixture-resource"]));
    assert_eq!(stored["refresh_token"], "fixture-refresh");
    assert_eq!(
        imported.credentials[0].upstream_user_id.as_deref(),
        Some("fixture-subject")
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert_eq!(
            request.headers["user-agent"],
            "antigravity/cli/1.5.15 (aidev_client; os_type=windows; arch=amd64; cl=1027820220; auth_method=consumer)"
        );
    }
    server.verify().await;
    server.reset().await;
    token(
        &server,
        json!({"access_token":"renewed-access","token_type":"Bearer","expires_in":3600}),
    )
    .await;
    let current = {
        let mut current = store.primary().loaded.lock().unwrap();
        current.credential = gateway_core::account::PlaintextCredential::new(stored);
        current.clone()
    };
    let refreshed = credentials.refresh_current(&current).await.unwrap();
    assert_eq!(
        refreshed
            .facts()
            .provider_material
            .clone()
            .into_provider_data()
            .expose_to_provider()["access_token"],
        "renewed-access"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers["user-agent"],
        "antigravity/hub/9.8.7 (aidev_client; os_type=linux; arch=amd64)"
    );
    let (_, guard) = refreshed.into_parts();
    guard.finish();
    assert_eq!(store.active_leases(), 0);
}

#[tokio::test]
async fn upstream_identity_failure_does_not_prepare_an_import() {
    let (server, credentials, _) = fixture().await;
    token(&server, token_response()).await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        credentials
            .prepare_import(import(document(file())))
            .await
            .unwrap_err()
            .kind(),
        Kind::Unavailable
    );
}

#[tokio::test]
async fn a_later_failed_grant_rejects_the_whole_import_without_exposing_tokens() {
    let (server, credentials, _) = fixture().await;
    let mut input = file();
    input["accounts"].as_array_mut().unwrap().push(json!({
        "label":"Second account", "refreshToken":"revoked-refresh",
    }));
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=fixture-refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response()))
        .expect(1)
        .mount(&server)
        .await;
    identity(&server, "fixture-subject").await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=revoked-refresh"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error":"invalid_grant", "error_description":"sensitive-response-marker",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let error = credentials
        .prepare_import(import(document(input)))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), Kind::CredentialRefreshRequired);
    assert!(!format!("{error:?}").contains("sensitive-response-marker"));
    assert!(!format!("{error:?}").contains("revoked-refresh"));
}

#[tokio::test]
async fn different_refresh_tokens_cannot_import_the_same_google_subject_twice() {
    let (server, credentials, _) = fixture().await;
    let mut input = file();
    input["accounts"].as_array_mut().unwrap().push(json!({
        "label":"Duplicate account", "refreshToken":"another-refresh",
    }));
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response()))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"same-subject"})))
        .expect(2)
        .mount(&server)
        .await;
    assert_eq!(
        credentials
            .prepare_import(import(document(input)))
            .await
            .unwrap_err()
            .kind(),
        Kind::Conflict
    );
}

#[tokio::test]
async fn rotation_keeps_subject_and_holds_leases_until_commit() {
    let (server, credentials, store) = fixture().await;
    token(&server, token_response()).await;
    identity(&server, "another-subject").await;
    let request = || PrepareCredentialRotation {
        account: account(),
        provider_material: document(file()["accounts"][0].clone()),
    };
    assert_eq!(
        credentials
            .prepare_rotation(request())
            .await
            .unwrap_err()
            .kind(),
        Kind::Conflict
    );
    assert_eq!(store.active_leases(), 0);
    server.reset().await;
    let mut response = token_response();
    response["refresh_token"] = json!("rotated-refresh");
    token(&server, response).await;
    identity(&server, "fixture-subject").await;
    let prepared = credentials.prepare_rotation(request()).await.unwrap();
    assert_eq!(store.active_leases(), 2);
    assert_eq!(prepared.facts().name, "Fixture");
    let (_, guard) = prepared.into_parts();
    guard.finish();
    assert_eq!(store.active_leases(), 0);
}

#[tokio::test]
async fn export_rejects_foreign_registration_or_mismatched_subject() {
    let (_, credentials, _) = fixture().await;
    for (registration, subject) in [
        ("wrong-client".to_owned(), "fixture-subject"),
        (credentials.oauth.registration(), "wrong-subject"),
    ] {
        let result = credentials.export_credentials(vec![ProviderExportCredentialInput {
            account: account(), provider_material: document(json!({"registration":registration,"subject":subject,"access_token":"access","refresh_token":"refresh","scopes":[]})),
        }]);
        assert_eq!(result.unwrap_err().kind(), Kind::Invalid);
    }
}
