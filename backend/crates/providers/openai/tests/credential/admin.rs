use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{TimeZone as _, Utc};
use provider_openai::credential::token_client::{
    OpenAiTokenClient, PersonalAccessTokenError, RefreshFailure, TokenClientConfig, TokenPair,
    TokenRefresher, openai_token_client,
};
use provider_openai::credential::{CodexCredentialAdminService, CodexCredentialCodec};
use secrecy::ExposeSecret as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

use crate::support::{TestLeaseCoordinator, runtime_policy};

struct UnusedRefresher;

#[tokio::test]
async fn sub2api_import_resolves_distinct_proxy_bindings_and_encodes_credentials() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service.prepare_import_document(serde_json::json!({"data": {
        "accounts": [
            {"name": "a", "platform": "openai", "type": "oauth", "proxy_key": "a", "credentials": {"access_token": test_jwt(serde_json::json!({"https://api.openai.com/auth": {"chatgpt_user_id": "user-a"}}))}},
            {"name": "b", "platform": "openai", "type": "oauth", "proxy_key": "b", "credentials": {"access_token": test_jwt(serde_json::json!({"https://api.openai.com/auth": {"chatgpt_user_id": "user-b"}}))}},
            {"name": "direct", "platform": "openai", "type": "oauth", "credentials": {"access_token": test_jwt(serde_json::json!({"https://api.openai.com/auth": {"chatgpt_user_id": "user-c"}}))}}
        ],
        "proxies": [
            {"proxy_key": "a", "protocol": "http", "host": "127.0.0.1", "port": 18080, "username": "user@a", "password": "p:a/ss", "status": "active"},
            {"proxy_key": "b", "protocol": "socks5", "host": "::1", "port": 1080, "status": "active", "fallback_mode": "none"}
        ]
    }})).await.unwrap();
    assert_eq!(prepared.accounts().len(), 3);
    let first = prepared.accounts()[0].account.outbound_proxy().unwrap();
    assert_eq!(first.endpoint(), "http://127.0.0.1:18080/");
    assert!(first.expose_url().contains("user%40a:p%3Aa%2Fss@"));
    assert_eq!(
        prepared.accounts()[1]
            .account
            .outbound_proxy()
            .unwrap()
            .endpoint(),
        "socks5h://[::1]:1080"
    );
    assert!(prepared.accounts()[2].account.outbound_proxy().is_none());
    assert!(!format!("{prepared:?}").contains("p:a/ss"));
}

#[tokio::test]
async fn import_default_proxy_preserves_explicit_account_exits() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let default_proxy =
        gateway_core::account::OutboundProxy::parse("http://default.example:8080").unwrap();
    for (field, value, expected) in [
        (
            None,
            serde_json::Value::Null,
            Some("http://default.example:8080/"),
        ),
        (
            Some("outboundProxyUrl"),
            serde_json::json!("socks5h://override.example:1080"),
            Some("socks5h://override.example:1080"),
        ),
        (Some("outbound_proxy_url"), serde_json::json!(""), None),
        (Some("outboundProxyUrl"), serde_json::Value::Null, None),
        (Some("proxy_key"), serde_json::Value::Null, None),
    ] {
        let mut account = serde_json::json!({"access_token": test_jwt(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_user_id": "default-proxy-user"}
        }))});
        if let Some(field) = field {
            account[field] = value;
        }
        let prepared = service
            .prepare_import_document_with_proxy(
                serde_json::json!({"data": {"accounts": [account]}}),
                Some(&default_proxy),
            )
            .await
            .unwrap();
        assert_eq!(
            prepared.accounts()[0]
                .account
                .outbound_proxy()
                .map(|proxy| proxy.endpoint())
                .as_deref(),
            expected
        );
    }
}

#[tokio::test]
async fn sub2api_invalid_proxy_bindings_are_rejected_before_any_token_refresh() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let proxy = serde_json::json!({"proxy_key": "bound", "protocol": "http", "host": "127.0.0.1", "port": 18080, "status": "active"});
    let mut cases = vec![
        serde_json::json!([]),
        serde_json::json!([proxy.clone(), proxy.clone()]),
    ];
    for (field, value) in [
        ("status", serde_json::json!("inactive")),
        ("expires_at", serde_json::json!(2000000000)),
        ("fallback_mode", serde_json::json!("direct")),
        ("port", serde_json::json!(0)),
    ] {
        let mut invalid = proxy.clone();
        invalid[field] = value;
        cases.push(serde_json::json!([invalid]));
    }
    for proxies in cases {
        let result = service.prepare_import_document(serde_json::json!({
            "accounts": [
                {"platform": "openai", "type": "oauth", "credentials": {"refresh_token": "must-not-refresh"}},
                {"platform": "openai", "type": "oauth", "proxy_key": "bound", "credentials": {"access_token": "at"}}
            ], "proxies": proxies
        })).await;
        assert!(result.is_err());
    }
}

#[async_trait]
impl TokenRefresher for UnusedRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        panic!("a direct access-token import must not refresh its refresh token")
    }
}

#[tokio::test]
async fn direct_import_persists_opaque_tokens_without_profile_or_token_validation() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": " ",
            "refreshToken": "",
            "idToken": "not.a.parseable.jwt"
        }))
        .await
        .expect("opaque direct import must be accepted");

    let account = prepared.accounts().first().expect("one prepared account");
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("stored credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(oauth.access_token.expose_secret(), " ");
    assert_eq!(
        oauth
            .refresh_token
            .as_ref()
            .expect("provided refresh token")
            .expose_secret(),
        ""
    );
    assert_eq!(
        oauth
            .id_token
            .as_ref()
            .expect("provided ID token")
            .expose_secret(),
        "not.a.parseable.jwt"
    );
    assert!(account.account.upstream_user_id().is_none());
    assert!(account.account.access_token_expires_at().is_none());
    assert!(account.account.next_refresh_at().is_none());
}

#[tokio::test]
async fn direct_import_accepts_snake_case_oauth_token_aliases() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accounts": [{
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "snake-access-token",
                    "refresh_token": "snake-refresh-token",
                    "id_token": "snake-id-token"
                }
            }]
        }))
        .await
        .expect("snake_case OAuth token aliases must be accepted");

    let account = prepared.accounts().first().expect("one prepared account");
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("stored credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(
        (
            oauth.access_token.expose_secret(),
            oauth
                .refresh_token
                .as_ref()
                .map(|token| token.expose_secret()),
            oauth.id_token.as_ref().map(|token| token.expose_secret()),
        ),
        (
            "snake-access-token",
            Some("snake-refresh-token"),
            Some("snake-id-token"),
        )
    );
}

#[tokio::test]
async fn direct_import_projects_access_token_jwt_expiry_without_persisting_refresh_margin() {
    let expires_at = Utc
        .timestamp_opt(2_000_000_000, 0)
        .single()
        .expect("valid test timestamp");
    let access_token = format!(
        "unverified-header.{}.unverified-signature",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({"exp": expires_at.timestamp()}))
                .expect("test JWT payload"),
        )
    );
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token,
            "refreshToken": "refresh-token"
        }))
        .await
        .expect("direct JWT import must be accepted");

    let account = prepared.accounts().first().expect("one prepared account");
    assert_eq!(
        account
            .account
            .access_token_expires_at()
            .map(chrono::DateTime::<Utc>::from),
        Some(expires_at)
    );
    assert!(account.account.next_refresh_at().is_none());
}

fn test_jwt(payload: serde_json::Value) -> String {
    format!(
        "unverified-header.{}.unverified-signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("test JWT payload"))
    )
}

fn pat_service(server: &MockServer) -> CodexCredentialAdminService {
    let client = openai_token_client(
        TokenClientConfig {
            client_id: "test-public-client".to_owned(),
            token_endpoint: format!("{}/oauth/token", server.uri()),
        },
        provider_openai::OpenAiConfig::default().wire_profile_state(),
    )
    .expect("auth client");
    CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    )
    .with_personal_access_token_client(Arc::new(client))
}

fn pat_identity() -> serde_json::Value {
    serde_json::json!({
        "email": "pat@example.com",
        "chatgpt_user_id": "pat-user",
        "chatgpt_account_id": "pat-workspace",
        "chatgpt_plan_type": "team",
        "chatgpt_account_is_fedramp": false
    })
}

#[tokio::test]
async fn pat_import_verifies_identity_and_becomes_schedulable_without_oauth_refresh_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/accounts/v1/user-auth-credential/whoami"))
        .and(header("authorization", "Bearer at-test-token"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pat_identity()))
        .expect(1)
        .mount(&server)
        .await;
    let service = pat_service(&server);
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accounts": [{
                "platform": "openai", "type": "codex", "name": "team PAT",
                "access_token": "  at-test-token  ",
                "refresh_token": "unrelated-refresh-token",
                "id_token": test_jwt(serde_json::json!({
                    "https://api.openai.com/auth": {"chatgpt_user_id": "untrusted-jwt-user"}
                })),
                "account_id": "untrusted-document-account",
                "email": "untrusted-document@example.com",
                "planType": "untrusted-plan", "expires_at": "2000-01-01T00:00:00Z"
            }]
        }))
        .await
        .expect("PAT import without a document user ID");
    let prepared = &prepared.accounts()[0];
    let account = &prepared.account;
    assert_eq!(account.upstream_user_id(), Some("pat-user"));
    assert_eq!(account.upstream_account_id(), Some("pat-workspace"));
    assert_eq!(account.email(), Some("pat@example.com"));
    assert_eq!(account.plan_type(), Some("team"));
    assert_eq!(account.name(), "team PAT");
    assert_eq!(
        account.credential_state(),
        gateway_core::account::CredentialState::Ready
    );
    assert_eq!(
        account.status_projection(SystemTime::now(), None).status,
        gateway_core::account::AccountStatus::Normal
    );
    assert!(!account.has_refresh_token());
    assert!(account.access_token_expires_at().is_none());
    assert!(account.next_refresh_at().is_none());
    let runtime = CodexCredentialCodec::decode(&prepared.credential).expect("stored PAT");
    let secret = runtime
        .authentication
        .oauth()
        .expect("shared Bearer transport");
    assert_eq!(secret.access_token.expose_secret(), "at-test-token");
    assert!(secret.refresh_token.is_none());
    assert!(secret.id_token.is_none());
    let now = SystemTime::now();
    let refresh_query = gateway_core::account::ProviderRefreshQuery::new(
        account.provider().clone(),
        now + Duration::from_secs(86_400),
        now + Duration::from_secs(86_400),
        now,
        Vec::new(),
        NonZeroU32::new(1).expect("nonzero limit"),
    );
    assert!(!refresh_query.contains(account));
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests[0].headers["originator"], "Codex Desktop");
    assert!(!requests[0].headers.contains_key("chatgpt-account-id"));
}

#[tokio::test]
async fn pat_import_times_out_without_falling_back_to_document_identity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(pat_identity())
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&server)
        .await;
    let client = OpenAiTokenClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(100))
            .build()
            .expect("short-lived test client"),
        TokenClientConfig {
            client_id: "test-public-client".to_owned(),
            token_endpoint: format!("{}/oauth/token", server.uri()),
        },
        provider_openai::OpenAiConfig::default().wire_profile_state(),
    );
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    )
    .with_personal_access_token_client(Arc::new(client));
    let error = service
        .prepare_import_document(serde_json::json!({
            "accessToken": "at-timeout-token", "userId": "untrusted-user"
        }))
        .await
        .expect_err("timeout must abort PAT import");
    assert_eq!(
        error,
        provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(
            PersonalAccessTokenError::Unavailable
        )
    );
}

#[tokio::test]
async fn pat_import_requires_a_validation_client() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let error = service
        .prepare_import_document(serde_json::json!({
            "accessToken": "at-test-token", "userId": "untrusted-user"
        }))
        .await
        .expect_err("PAT cannot be imported without validation");
    assert_eq!(
        error,
        provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(
            PersonalAccessTokenError::Unavailable
        )
    );
}

#[tokio::test]
async fn pat_auth_json_import_and_cpr_round_trip_preserve_a_token_without_email() {
    use gateway_core::account::LoadedCredential;
    use provider_openai::credential::{CodexCredentialAdmin, ExportManagedCodexCredential};

    let server = MockServer::start().await;
    let mut identity = pat_identity();
    identity.as_object_mut().expect("object").remove("email");
    Mock::given(method("GET"))
        .and(path("/api/accounts/v1/user-auth-credential/whoami"))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity))
        .expect(2)
        .mount(&server)
        .await;
    let service = pat_service(&server);
    let imported = service
        .prepare_import_document(serde_json::json!({
            "auth_mode": "personalAccessToken", "personal_access_token": "at-test-token"
        }))
        .await
        .expect("official PAT auth.json");
    let prepared = imported.into_accounts().remove(0);
    assert!(prepared.account.email().is_none());
    let exported = CodexCredentialAdmin
        .format_cpr_export(vec![ExportManagedCodexCredential {
            current: LoadedCredential {
                account: prepared.account,
                credential: prepared.credential,
            },
            added_at: Utc::now(),
            updated_at: Utc::now(),
        }])
        .expect("export PAT using the existing credential schema");
    let imported = service
        .prepare_import_document(serde_json::to_value(exported).expect("export JSON"))
        .await
        .expect("re-import verifies PAT again");
    assert_eq!(
        imported.accounts()[0].account.upstream_user_id(),
        Some("pat-user")
    );
    assert!(!imported.accounts()[0].account.has_refresh_token());
}

#[tokio::test]
async fn pat_import_fails_closed_for_rejection_and_unavailable_upstream_without_leaking_secrets() {
    for (status, expected) in [
        (401, PersonalAccessTokenError::Rejected),
        (403, PersonalAccessTokenError::Rejected),
        (429, PersonalAccessTokenError::Unavailable),
        (500, PersonalAccessTokenError::Unavailable),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status).set_body_string("pat-response-secret-marker"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = pat_service(&server)
            .prepare_import_document(serde_json::json!({
                "accessToken": "at-request-secret-marker", "userId": "document-user"
            }))
            .await
            .expect_err("no account may be prepared after failed validation");
        assert_eq!(
            error,
            provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(expected)
        );
        let message = format!("{error} {error:?}");
        assert!(!message.contains("pat-response-secret-marker"));
        assert!(!message.contains("at-request-secret-marker"));
    }
}

#[tokio::test]
async fn pat_import_rejects_missing_or_invalid_whoami_identity_fields() {
    for field in [
        "chatgpt_user_id",
        "chatgpt_account_id",
        "chatgpt_plan_type",
        "chatgpt_account_is_fedramp",
    ] {
        for value in [
            None,
            Some(serde_json::json!("")),
            Some(serde_json::json!(null)),
        ] {
            let server = MockServer::start().await;
            let mut identity = pat_identity();
            if let Some(value) = value {
                identity[field] = value;
            } else {
                identity.as_object_mut().expect("object").remove(field);
            }
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(identity))
                .expect(1)
                .mount(&server)
                .await;
            let error = pat_service(&server)
                .prepare_import_document(serde_json::json!({
                    "access_token": "at-test-token", "user_id": "not-a-fallback"
                }))
                .await
                .expect_err("malformed whoami must not fall back to document identity");
            assert_eq!(
                error,
                provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(
                    PersonalAccessTokenError::InvalidResponse
                )
            );
        }
    }
}

#[tokio::test]
async fn pat_import_rejects_invalid_json_and_oversized_chunked_success_bodies() {
    for body in ["not-json-secret-marker".to_owned(), "x".repeat(70 * 1024)] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("transfer-encoding", "chunked")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = pat_service(&server)
            .prepare_import_document(serde_json::json!({"accessToken": "at-test-token"}))
            .await
            .expect_err("invalid body");
        assert_eq!(
            error,
            provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(
                PersonalAccessTokenError::InvalidResponse
            )
        );
        assert!(!format!("{error:?}").contains("secret-marker"));
    }
}

#[tokio::test]
async fn pat_import_does_not_follow_redirects_or_forward_bearer_to_another_endpoint() {
    let server = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", destination.uri()))
        .expect(1)
        .mount(&server)
        .await;
    let error = pat_service(&server)
        .prepare_import_document(serde_json::json!({"accessToken": "at-test-token"}))
        .await
        .expect_err("redirects are not accepted");
    assert_eq!(
        error,
        provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(
            PersonalAccessTokenError::Unavailable
        )
    );
    assert!(
        destination
            .received_requests()
            .await
            .expect("redirect target")
            .is_empty()
    );
}

#[tokio::test]
async fn pat_import_rejects_malformed_tokens_before_sending_a_request() {
    let server = MockServer::start().await;
    let service = pat_service(&server);
    for token in ["at-", "at-has space", "at-has\nnewline", "at-has\0control"] {
        let error = service
            .prepare_import_document(serde_json::json!({"accessToken": token}))
            .await
            .expect_err("invalid PAT");
        assert_eq!(
            error,
            provider_openai::credential::CodexCredentialAdminError::PersonalAccessToken(
                PersonalAccessTokenError::InvalidToken
            )
        );
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn ordinary_jwt_import_does_not_call_pat_whoami_even_when_client_is_configured() {
    let server = MockServer::start().await;
    let jwt = test_jwt(
        serde_json::json!({"https://api.openai.com/auth": {"chatgpt_user_id": "jwt-user"}}),
    );
    let imported = pat_service(&server)
        .prepare_import_document(serde_json::json!({
            "accessToken": jwt, "refreshToken": "jwt-refresh", "userId": "untrusted"
        }))
        .await
        .expect("existing JWT behavior");
    assert_eq!(
        imported.accounts()[0].account.upstream_user_id(),
        Some("jwt-user")
    );
    assert!(imported.accounts()[0].account.has_refresh_token());
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn oauth_import_uses_official_chatgpt_access_token_claims() {
    let access_token = test_jwt(serde_json::json!({
        "email": "top-level@example.com",
        "https://api.openai.com/profile": {"email": "profile@example.com"},
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_user_id": "chatgpt-user",
            "user_id": "fallback-user",
            "chatgpt_account_id": "chatgpt-account"
        }
    }));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token
        }))
        .await
        .expect("official access token claims are accepted");

    let account = &prepared.accounts().first().expect("one account").account;
    assert_eq!(account.upstream_user_id(), Some("chatgpt-user"));
    assert_eq!(account.upstream_account_id(), Some("chatgpt-account"));
    assert_eq!(account.email(), Some("top-level@example.com"));
    assert_eq!(account.plan_type(), Some("pro"));
}

#[tokio::test]
async fn oauth_import_uses_official_plan_alias_projection_and_user_id_fallback() {
    let access_token = test_jwt(serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "hc",
            "user_id": "fallback-user",
            "chatgpt_account_is_fedramp": false
        }
    }));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token
        }))
        .await
        .expect("official plan alias and user ID fallback are accepted");

    let account = &prepared.accounts().first().expect("one account").account;
    assert_eq!(account.upstream_user_id(), Some("fallback-user"));
    assert_eq!(account.plan_type(), Some("enterprise"));
}

#[tokio::test]
async fn oauth_import_uses_id_token_then_access_token_for_missing_claims() {
    let id_token = test_jwt(serde_json::json!({
        "https://api.openai.com/profile": {"email": "id-token@example.com"},
        "https://api.openai.com/auth": {"chatgpt_user_id": "id-token-user"}
    }));
    let access_token = test_jwt(serde_json::json!({
        "email": "access-token@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "plus",
            "chatgpt_user_id": "access-token-user",
            "chatgpt_account_id": "access-token-account"
        }
    }));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token,
            "idToken": id_token
        }))
        .await
        .expect("ID token and access token claims are accepted");

    let account = &prepared.accounts().first().expect("one account").account;
    assert_eq!(account.upstream_user_id(), Some("id-token-user"));
    assert_eq!(account.upstream_account_id(), Some("access-token-account"));
    assert_eq!(account.email(), Some("id-token@example.com"));
    assert_eq!(account.plan_type(), Some("plus"));
}

#[tokio::test]
async fn oauth_import_does_not_use_top_level_identity_fields() {
    let access_token = test_jwt(serde_json::json!({"exp": 2_000_000_000_i64}));
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(serde_json::json!({
            "accessToken": access_token,
            "userId": "untrusted-user",
            "accountId": "untrusted-account",
            "email": "untrusted@example.com",
            "planType": "pro"
        }))
        .await
        .expect("token without identity claims remains importable");

    let account = &prepared.accounts().first().expect("one account").account;
    assert!(account.upstream_user_id().is_none());
    assert!(account.upstream_account_id().is_none());
    assert!(account.email().is_none());
    assert!(account.plan_type().is_none());
}

#[tokio::test]
async fn oauth_import_rejects_legacy_bare_token_field() {
    let service = CodexCredentialAdminService::new(
        Arc::new(UnusedRefresher),
        Arc::new(TestLeaseCoordinator::default()),
        runtime_policy(),
    );
    let error = service
        .prepare_import_document(serde_json::json!({
            "token": "header.payload.signature"
        }))
        .await
        .expect_err("ambiguous token field must not be accepted");

    assert_eq!(
        error,
        provider_openai::credential::CodexCredentialAdminError::InvalidCredential
    );
}

#[tokio::test]
async fn pat_import_uses_account_proxy_without_direct_fallback() {
    for (status, use_default) in [(200, false), (503, false), (200, true), (503, true)] {
        let origin = MockServer::start().await;
        let proxy = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/accounts/v1/user-auth-credential/whoami"))
            .and(header("authorization", "Bearer at-proxy-token"))
            .respond_with(ResponseTemplate::new(status).set_body_json(pat_identity()))
            .expect(1)
            .mount(&proxy)
            .await;
        let default_proxy = gateway_core::account::OutboundProxy::parse(&proxy.uri()).unwrap();
        let mut document = serde_json::json!({"accessToken": "at-proxy-token"});
        if !use_default {
            document["outboundProxyUrl"] = serde_json::json!(proxy.uri());
        }
        let imported = pat_service(&origin)
            .prepare_import_document_with_proxy(document, use_default.then_some(&default_proxy))
            .await;
        if status == 200 {
            let prepared = imported.expect("PAT validated through account proxy");
            assert_eq!(
                prepared.accounts()[0].account.upstream_user_id(),
                Some("pat-user")
            );
            assert!(prepared.accounts()[0].account.outbound_proxy().is_some());
        } else {
            assert!(imported.is_err(), "proxy failure must abort PAT import");
        }
        assert!(
            origin
                .received_requests()
                .await
                .expect("origin requests")
                .is_empty()
        );
    }
}
