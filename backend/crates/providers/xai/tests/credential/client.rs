//! External fixture-driven contracts for the transport and verification ports.

use std::collections::VecDeque;
use std::future::ready;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gateway_core::engine::credential::AccountAvailability;
use provider_xai::{
    AuthorizationCallback, FailClosedTokenVerifier, FailureClass, FormValue, GrokCredentialAdmin,
    GrokOAuthClient, GrokOAuthConfig, GrokOAuthImportCandidate, GrokOAuthImportMetadata,
    GrokOAuthImportTokens, OAuthError, OAuthHttpRequest, OAuthHttpResponse, OAuthHttpTransport,
    RedirectUriAllowlist, RefreshTokenGrant, SecretValue, TokenCandidate, TokenVerificationContext,
    TokenVerifier, TransportFailure, TransportFailureKind, TransportFuture, VerificationEvidence,
    VerificationFailure, VerificationFlow, VerificationFuture, VerificationMethod,
    VerifiedGrokAccount,
};

use crate::support::{account_id, instance_id};

const DISCOVERY: &[u8] = include_bytes!("fixtures/discovery.json");
const TOKEN_SUCCESS: &[u8] = include_bytes!("fixtures/token_success.json");
const REFRESH_SUCCESS: &[u8] = include_bytes!("fixtures/refresh_success.json");

struct FixtureTransport {
    responses: Mutex<VecDeque<Result<OAuthHttpResponse, TransportFailure>>>,
    requests: Mutex<Vec<OAuthHttpRequest>>,
}

impl FixtureTransport {
    fn new(responses: Vec<Result<OAuthHttpResponse, TransportFailure>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<OAuthHttpRequest> {
        self.requests
            .lock()
            .expect("fixture request lock is not poisoned")
            .clone()
    }
}

impl OAuthHttpTransport for FixtureTransport {
    fn execute(&self, request: OAuthHttpRequest) -> TransportFuture<'_> {
        self.requests
            .lock()
            .expect("fixture request lock is not poisoned")
            .push(request);
        let result = self
            .responses
            .lock()
            .expect("fixture response lock is not poisoned")
            .pop_front()
            .expect("fixture response queue has an entry");
        Box::pin(ready(result))
    }
}

#[derive(Debug)]
struct FixtureVerifier;

impl TokenVerifier for FixtureVerifier {
    fn verify<'a>(
        &'a self,
        context: TokenVerificationContext<'a>,
        candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a> {
        Box::pin(async move {
            match context.flow() {
                VerificationFlow::AuthorizationCode => {
                    if context.expected_nonce().is_none() || candidate.id_token().is_none() {
                        return Err(VerificationFailure::Rejected);
                    }
                    Ok(VerificationEvidence::id_token("fixture-subject".to_owned()))
                }
                VerificationFlow::CredentialImport
                | VerificationFlow::CredentialImportRefreshed => Ok(
                    VerificationEvidence::user_info("fixture-subject".to_owned()),
                ),
            }
        })
    }
}

#[tokio::test]
async fn credential_import_should_require_exact_metadata_and_official_userinfo() {
    let transport = Arc::new(FixtureTransport::new(vec![response(200, DISCOVERY)]));
    let client = GrokOAuthClient::new(config(), transport, Arc::new(FixtureVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let now = chrono::Utc::now();
    let candidate = GrokOAuthImportCandidate::new(
        GrokOAuthImportTokens::new(
            SecretValue::new("fixture-access-token".to_owned()),
            SecretValue::new("fixture-refresh-token".to_owned()),
            SecretValue::new("header.payload.signature".to_owned()),
        ),
        GrokOAuthImportMetadata::new(
            "Bearer".to_owned(),
            client.config().client_id().to_owned(),
            "openid offline_access grok-cli:access api:access".to_owned(),
            provider_xai::GROK_CLI_BASE_URL.to_owned(),
            now,
            now + chrono::Duration::hours(1),
        ),
    );

    let verified = client
        .verify_imported_credential(&discovery, candidate)
        .await
        .expect("strict imported credential should verify");

    assert_eq!(verified.evidence().method(), VerificationMethod::UserInfo);
    let prepared = GrokCredentialAdmin
        .prepare_verified_account(&VerifiedGrokAccount {
            account_id: account_id("verified-import"),
            provider_instance_id: instance_id(),
            name: "verified import".to_owned(),
            email: Some("verified@example.com".to_owned()),
            upstream_account_id: None,
            plan_type: None,
            tokens: verified,
            enabled: true,
            refresh_margin: Duration::from_secs(300),
        })
        .expect("Provider projects verified token lifetime");
    assert_eq!(prepared.account.availability(), AccountAvailability::Ready);
    assert!(prepared.account.next_refresh_at().is_some());
    assert!(
        prepared
            .credential
            .expose_to_provider()
            .contains_key("id_token")
    );
    assert_eq!(
        prepared
            .credential
            .expose_to_provider()
            .get("scope")
            .and_then(serde_json::Value::as_str),
        Some("openid offline_access grok-cli:access api:access")
    );
}

#[tokio::test]
async fn expired_import_should_refresh_then_require_authoritative_userinfo() {
    let transport = Arc::new(FixtureTransport::new(vec![
        response(200, DISCOVERY),
        response(200, REFRESH_SUCCESS),
    ]));
    let client = GrokOAuthClient::new(config(), transport.clone(), Arc::new(FixtureVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let now = chrono::Utc::now();
    let candidate = GrokOAuthImportCandidate::new(
        GrokOAuthImportTokens::new(
            SecretValue::new("expired-access-token".to_owned()),
            SecretValue::new("import-refresh-token".to_owned()),
            SecretValue::new("expired.header.signature".to_owned()),
        ),
        GrokOAuthImportMetadata::new(
            "Bearer".to_owned(),
            client.config().client_id().to_owned(),
            "openid offline_access grok-cli:access api:access".to_owned(),
            provider_xai::GROK_CLI_BASE_URL.to_owned(),
            now - chrono::Duration::hours(2),
            now - chrono::Duration::seconds(1),
        ),
    );

    let verified = client
        .verify_imported_credential(&discovery, candidate)
        .await
        .expect("expired import refreshes and verifies");

    assert_eq!(verified.evidence().method(), VerificationMethod::UserInfo);
    assert!(verified.refresh_token().is_some());
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert!(matches!(
        requests[1]
            .form()
            .iter()
            .find(|field| field.name() == "refresh_token")
            .map(|field| field.value()),
        Some(FormValue::Secret(_))
    ));
}

#[tokio::test]
async fn credential_import_should_reject_non_official_client_before_verification() {
    let transport = Arc::new(FixtureTransport::new(vec![response(200, DISCOVERY)]));
    let client = GrokOAuthClient::new(config(), transport, Arc::new(FixtureVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let now = chrono::Utc::now();
    let candidate = GrokOAuthImportCandidate::new(
        GrokOAuthImportTokens::new(
            SecretValue::new("fixture-access-token".to_owned()),
            SecretValue::new("fixture-refresh-token".to_owned()),
            SecretValue::new("header.payload.signature".to_owned()),
        ),
        GrokOAuthImportMetadata::new(
            "Bearer".to_owned(),
            "another-client".to_owned(),
            "openid offline_access grok-cli:access api:access".to_owned(),
            provider_xai::GROK_CLI_BASE_URL.to_owned(),
            now,
            now + chrono::Duration::hours(1),
        ),
    );

    let error = client
        .verify_imported_credential(&discovery, candidate)
        .await
        .expect_err("another client must be rejected");

    assert!(matches!(
        error,
        provider_xai::GrokOAuthImportError::InvalidField("client_id")
    ));
}

fn response(status: u16, body: &[u8]) -> Result<OAuthHttpResponse, TransportFailure> {
    Ok(OAuthHttpResponse::new(status, body.to_vec()))
}

fn config() -> GrokOAuthConfig {
    GrokOAuthConfig::official("0.2.101").expect("fixture config is valid")
}

fn state_from_authorization_url(pending: &provider_xai::PendingAuthorization) -> String {
    pending
        .authorization_url()
        .query_pairs()
        .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
        .expect("authorization URL contains state")
}

#[tokio::test]
async fn authorization_code_flow_should_verify_before_returning_tokens() {
    let transport = Arc::new(FixtureTransport::new(vec![
        response(200, DISCOVERY),
        response(200, TOKEN_SUCCESS),
    ]));
    let client = GrokOAuthClient::new(config(), transport.clone(), Arc::new(FixtureVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let allowlist = RedirectUriAllowlist::new(["https://gateway.example/oauth/grok/callback"])
        .expect("fixture allowlist is valid");
    let redirect = allowlist
        .authorize("https://gateway.example/oauth/grok/callback")
        .expect("fixture callback is allowlisted");
    let pending = client
        .start_authorization_code(&discovery, redirect, None)
        .expect("authorization starts");
    let state = state_from_authorization_url(&pending);
    let callback = AuthorizationCallback::parse(&format!("code=fixture-code&state={state}"))
        .expect("fixture callback parses");
    let grant = pending
        .accept_callback(callback)
        .expect("callback state is valid");

    let tokens = client
        .exchange_authorization_code(&discovery, grant)
        .await
        .expect("verified token exchange succeeds");

    assert_eq!(tokens.evidence().method(), VerificationMethod::IdToken);
    assert_eq!(transport.requests()[1].url().path(), "/oauth2/token");
    assert!(matches!(
        transport.requests()[1]
            .form()
            .iter()
            .find(|field| field.name() == "code")
            .map(|field| field.value()),
        Some(FormValue::Secret(_))
    ));
}

#[tokio::test]
async fn authorization_code_flow_should_fail_closed_without_verifier() {
    let transport = Arc::new(FixtureTransport::new(vec![
        response(200, DISCOVERY),
        response(200, TOKEN_SUCCESS),
    ]));
    let client = GrokOAuthClient::new(config(), transport, Arc::new(FailClosedTokenVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let allowlist = RedirectUriAllowlist::new(["https://gateway.example/oauth/grok/callback"])
        .expect("fixture allowlist is valid");
    let redirect = allowlist
        .authorize("https://gateway.example/oauth/grok/callback")
        .expect("fixture callback is allowlisted");
    let pending = client
        .start_authorization_code(&discovery, redirect, None)
        .expect("authorization starts");
    let state = state_from_authorization_url(&pending);
    let callback = AuthorizationCallback::parse(&format!("code=fixture-code&state={state}"))
        .expect("fixture callback parses");
    let grant = pending
        .accept_callback(callback)
        .expect("callback state is valid");

    let error = client
        .exchange_authorization_code(&discovery, grant)
        .await
        .expect_err("unwired verification must fail closed");

    assert!(matches!(
        error,
        OAuthError::Verification(VerificationFailure::Unavailable)
    ));
}

#[tokio::test]
async fn refresh_should_preserve_ambiguous_send_classification() {
    let transport = Arc::new(FixtureTransport::new(vec![
        response(200, DISCOVERY),
        Err(TransportFailure::new(TransportFailureKind::Ambiguous)),
    ]));
    let client = GrokOAuthClient::new(config(), transport, Arc::new(FailClosedTokenVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let grant = RefreshTokenGrant::new(SecretValue::new(
        "fixture-refresh-token-not-usable".to_owned(),
    ));

    let error = client
        .refresh(&discovery, &grant)
        .await
        .expect_err("ambiguous refresh must surface");

    assert_eq!(error.class(), FailureClass::Ambiguous);
}

#[tokio::test]
async fn refresh_should_return_rotated_token_without_id_token() {
    let transport = Arc::new(FixtureTransport::new(vec![
        response(200, DISCOVERY),
        response(200, REFRESH_SUCCESS),
    ]));
    let client = GrokOAuthClient::new(config(), transport, Arc::new(FailClosedTokenVerifier));
    let discovery = client.discover().await.expect("discovery succeeds");
    let grant = RefreshTokenGrant::new(SecretValue::new(
        "fixture-refresh-token-not-usable".to_owned(),
    ));

    let refreshed = client
        .refresh(&discovery, &grant)
        .await
        .expect("fixture refresh succeeds");

    assert_eq!(
        refreshed
            .rotated_refresh_token()
            .expect("fixture rotates refresh token")
            .expose(),
        "fixture-rotated-refresh-token-not-usable"
    );
}
