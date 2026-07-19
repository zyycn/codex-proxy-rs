use std::future::ready;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration as StdDuration, SystemTime};

use chrono::{Duration, Utc};
use provider_xai::{
    FailClosedTokenVerifier, FailureClass, GrokCredentialAdmin, GrokOAuthClient, GrokOAuthConfig,
    GrokOAuthImportCandidate, GrokOAuthImportDocument, GrokOAuthImportError,
    GrokOAuthImportMetadata, GrokOAuthImportTokens, OAuthHttpRequest, OAuthHttpResponse,
    OAuthHttpTransport, ReqwestOAuthTransport, ReqwestOidcTokenVerifier, SecretValue,
    TokenCandidate, TokenVerificationContext, TokenVerifier, TransportFuture, VerificationEvidence,
    VerificationFuture, VerifiedGrokAccount,
};

use crate::support::{account_id, instance_id};

const DISCOVERY: &[u8] = include_bytes!("fixtures/discovery.json");
const INVALID_GRANT: &[u8] = include_bytes!("fixtures/invalid_grant.json");
const REQUIRED_SCOPE: &str = "openid offline_access grok-cli:access api:access";

struct DiscoveryTransport;

impl OAuthHttpTransport for DiscoveryTransport {
    fn execute(&self, _request: OAuthHttpRequest) -> TransportFuture<'_> {
        Box::pin(ready(Ok(OAuthHttpResponse::new(200, DISCOVERY.to_vec()))))
    }
}

struct AcceptingUserInfoVerifier;

impl TokenVerifier for AcceptingUserInfoVerifier {
    fn verify<'a>(
        &'a self,
        _context: TokenVerificationContext<'a>,
        _candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a> {
        Box::pin(ready(Ok(VerificationEvidence::user_info(
            "fixture-subject".to_owned(),
        ))))
    }
}

#[derive(Default)]
struct RejectingRefreshTransport {
    calls: AtomicUsize,
}

impl OAuthHttpTransport for RejectingRefreshTransport {
    fn execute(&self, _request: OAuthHttpRequest) -> TransportFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(ready(Ok(if call == 0 {
            OAuthHttpResponse::new(200, DISCOVERY.to_vec())
        } else {
            OAuthHttpResponse::new(400, INVALID_GRANT.to_vec())
        })))
    }
}

#[tokio::test]
async fn expired_access_token_requires_official_refresh_before_identity_verification() {
    let now = Utc::now();
    let transport = Arc::new(RejectingRefreshTransport::default());
    let client = GrokOAuthClient::new(
        GrokOAuthConfig::official("0.2.101").expect("official config"),
        transport.clone(),
        Arc::new(FailClosedTokenVerifier),
    );
    let discovery = client.discover().await.expect("official discovery fixture");
    let error = client
        .verify_imported_credential(
            &discovery,
            candidate(
                provider_xai::GROK_CLI_BASE_URL,
                REQUIRED_SCOPE,
                now - Duration::hours(2),
                now - Duration::seconds(1),
            ),
        )
        .await
        .expect_err("rejected RT must fail before identity verification");

    assert!(matches!(error, GrokOAuthImportError::OAuth(_)));
    assert_eq!(error.class(), FailureClass::CredentialPermanent);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn non_official_base_url_should_fail_closed() {
    let now = Utc::now();
    let error = verify(candidate(
        "https://api.x.ai/v1",
        REQUIRED_SCOPE,
        now,
        now + Duration::hours(1),
    ))
    .await;

    assert!(matches!(
        error,
        GrokOAuthImportError::InvalidField("base_url")
    ));
}

#[tokio::test]
async fn missing_required_scope_should_fail_closed() {
    let now = Utc::now();
    let error = verify(candidate(
        provider_xai::GROK_CLI_BASE_URL,
        "openid offline_access grok-cli:access",
        now,
        now + Duration::hours(1),
    ))
    .await;

    assert!(matches!(error, GrokOAuthImportError::InvalidField("scope")));
}

#[tokio::test]
async fn verified_import_inside_refresh_margin_should_schedule_immediate_refresh() {
    let now = Utc::now();
    let client = GrokOAuthClient::new(
        GrokOAuthConfig::official("0.2.101").expect("official config"),
        Arc::new(DiscoveryTransport),
        Arc::new(AcceptingUserInfoVerifier),
    );
    let discovery = client.discover().await.expect("official discovery fixture");
    let tokens = client
        .verify_imported_credential(
            &discovery,
            candidate(
                provider_xai::GROK_CLI_BASE_URL,
                REQUIRED_SCOPE,
                now,
                now + Duration::minutes(30),
            ),
        )
        .await
        .expect("still-valid imported credential");
    let before = SystemTime::now();
    let prepared = GrokCredentialAdmin
        .prepare_verified_account(&VerifiedGrokAccount {
            account_id: account_id("refresh-window-import"),
            provider_instance_id: instance_id(),
            name: "refresh-window-import".to_owned(),
            email: None,
            upstream_account_id: None,
            plan_type: None,
            tokens,
            enabled: true,
            refresh_margin: StdDuration::from_secs(60 * 60),
        })
        .expect("valid credential inside refresh window must be imported");
    let after = SystemTime::now();
    let next_refresh_at = prepared
        .account
        .next_refresh_at()
        .expect("refreshable credential must be scheduled");

    assert!((before..=after).contains(&next_refresh_at));
}

#[test]
fn candidate_debug_should_redact_all_identity_and_secret_material() {
    let now = Utc::now();
    let candidate = GrokOAuthImportCandidate::new(
        GrokOAuthImportTokens::new(
            SecretValue::new("debug-access-secret".to_owned()),
            SecretValue::new("debug-refresh-secret".to_owned()),
            SecretValue::new("debug-id-secret".to_owned()),
        ),
        GrokOAuthImportMetadata::new(
            "Bearer".to_owned(),
            "debug-client-secret".to_owned(),
            "debug-scope-secret".to_owned(),
            provider_xai::GROK_CLI_BASE_URL.to_owned(),
            now,
            now + Duration::hours(1),
        ),
    );

    let debug = format!("{candidate:?}");
    assert!(debug.contains("[REDACTED]"));
    for forbidden in [
        "debug-access-secret",
        "debug-refresh-secret",
        "debug-id-secret",
        "debug-client-secret",
        "debug-scope-secret",
    ] {
        assert!(!debug.contains(forbidden));
    }
}

#[test]
fn strict_oauth_account_document_should_parse_without_exposing_credentials() {
    let document = oauth_account_document();
    let parsed = GrokOAuthImportDocument::parse_json(&document).expect("strict document");
    let debug = format!("{parsed:?}");
    assert!(debug.contains("account_count"));
    assert!(!debug.contains("fixture-access-token"));
    assert!(!debug.contains("fixture-refresh-token"));

    let mut entries = parsed.into_entries();
    assert_eq!(entries.len(), 1);
    let entry = entries.pop().expect("one account");
    assert_eq!(entry.name(), "grok-primary");
    assert_eq!(entry.email(), Some("owner@example.test"));
    let entry_debug = format!("{entry:?}");
    assert!(!entry_debug.contains("owner@example.test"));
    assert!(!entry_debug.contains("fixture-access-token"));
    let _candidate = entry.into_candidate();
}

#[test]
fn oauth_account_document_should_reject_api_key_credential_field() {
    let mut document: serde_json::Value =
        serde_json::from_slice(&oauth_account_document()).expect("fixture JSON");
    document["accounts"][0]["credentials"]["api_key"] =
        serde_json::Value::String("must-not-be-accepted".to_owned());

    let error = GrokOAuthImportDocument::parse_json(
        &serde_json::to_vec(&document).expect("serialize fixture"),
    )
    .expect_err("API key is outside the OAuth contract");
    assert!(matches!(
        error,
        GrokOAuthImportError::InvalidField("account")
    ));
}

#[test]
fn oauth_account_document_should_accept_optional_header_and_dynamic_provider_fields() {
    let mut document: serde_json::Value =
        serde_json::from_slice(&oauth_account_document()).expect("fixture JSON");
    document["type"] = serde_json::Value::String("external-account-bundle".to_owned());
    document["version"] = serde_json::Value::from(1);
    document["skipped_shadows"] = serde_json::Value::from(2);
    document["accounts"][0]["credentials"]["provider_extension"] =
        serde_json::json!({"window": "dynamic"});
    document["accounts"][0]["extra"]["provider_snapshot"] = serde_json::json!({"remaining": 1});

    let parsed = GrokOAuthImportDocument::parse_json(
        &serde_json::to_vec(&document).expect("serialize fixture"),
    )
    .expect("official optional fields");

    assert_eq!(parsed.into_entries().len(), 1);
}

#[test]
fn strict_oauth_account_document_should_reject_proxy_and_identity_mismatch() {
    let mut proxy_document: serde_json::Value =
        serde_json::from_slice(&oauth_account_document()).expect("fixture JSON");
    proxy_document["proxies"] = serde_json::json!([{"url": "http://127.0.0.1:8080"}]);
    assert!(
        GrokOAuthImportDocument::parse_json(
            &serde_json::to_vec(&proxy_document).expect("serialize fixture")
        )
        .is_err()
    );

    let mut mismatch_document: serde_json::Value =
        serde_json::from_slice(&oauth_account_document()).expect("fixture JSON");
    mismatch_document["accounts"][0]["extra"]["email"] =
        serde_json::Value::String("different@example.test".to_owned());
    let error = GrokOAuthImportDocument::parse_json(
        &serde_json::to_vec(&mismatch_document).expect("serialize fixture"),
    )
    .expect_err("identity mismatch must fail closed");
    assert!(matches!(error, GrokOAuthImportError::InvalidField("email")));
}

#[test]
fn strict_oauth_account_document_should_reject_duplicate_account_names() {
    let mut document: serde_json::Value =
        serde_json::from_slice(&oauth_account_document()).expect("fixture JSON");
    let duplicate = document["accounts"][0].clone();
    document["accounts"]
        .as_array_mut()
        .expect("accounts array")
        .push(duplicate);

    let error = GrokOAuthImportDocument::parse_json(
        &serde_json::to_vec(&document).expect("serialize fixture"),
    )
    .expect_err("duplicate names must be rejected");
    assert!(matches!(
        error,
        GrokOAuthImportError::InvalidField("account")
    ));
}

#[test]
#[ignore = "requires XAI_REAL_ACCOUNT_FIXTURE"]
fn real_oauth_account_should_match_provider_import_contract() {
    let path = std::env::var_os("XAI_REAL_ACCOUNT_FIXTURE")
        .expect("XAI_REAL_ACCOUNT_FIXTURE must point to a local secret fixture");
    let document = std::fs::read(path).expect("read local secret fixture");
    let expected_accounts = serde_json::from_slice::<serde_json::Value>(&document)
        .expect("parse local secret fixture")
        .get("accounts")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .filter(|count| *count > 0)
        .expect("fixture must contain accounts");
    let parsed = GrokOAuthImportDocument::parse_json(&document).expect("strict Provider import");

    assert_eq!(parsed.into_entries().len(), expected_accounts);
}

#[tokio::test]
#[ignore = "requires XAI_REAL_ACCOUNT_FIXTURE and official xAI network access"]
async fn real_oauth_accounts_should_cross_the_official_verification_boundary() {
    let path = std::env::var_os("XAI_REAL_ACCOUNT_FIXTURE")
        .expect("XAI_REAL_ACCOUNT_FIXTURE must point to a local secret fixture");
    let document = std::fs::read(path).expect("read local secret fixture");
    let entries = GrokOAuthImportDocument::parse_json(&document)
        .expect("real document must match the import contract")
        .into_entries();
    let expected_accounts = entries.len();
    let client = GrokOAuthClient::new(
        GrokOAuthConfig::official("0.2.101").expect("official config"),
        Arc::new(ReqwestOAuthTransport::new().expect("production OAuth transport")),
        Arc::new(ReqwestOidcTokenVerifier::new().expect("production token verifier")),
    );
    let discovery = client.discover().await.expect("official discovery");
    let mut prepared_accounts = 0_usize;
    for (index, entry) in entries.into_iter().enumerate() {
        let tokens = client
            .verify_imported_credential(&discovery, entry.into_candidate())
            .await
            .expect("real OAuth credential must pass official verification");
        let suffix = format!("real-import-{index}");
        GrokCredentialAdmin
            .prepare_verified_account(&VerifiedGrokAccount {
                account_id: account_id(&suffix),
                provider_instance_id: instance_id(),
                name: suffix,
                email: None,
                upstream_account_id: None,
                plan_type: None,
                tokens,
                enabled: true,
                refresh_margin: StdDuration::from_secs(60 * 60),
            })
            .expect("verified real credential must cross account preparation");
        prepared_accounts += 1;
    }

    assert_eq!(prepared_accounts, expected_accounts);
}

async fn verify(candidate: GrokOAuthImportCandidate) -> GrokOAuthImportError {
    let client = GrokOAuthClient::new(
        GrokOAuthConfig::official("0.2.101").expect("official config"),
        Arc::new(DiscoveryTransport),
        Arc::new(FailClosedTokenVerifier),
    );
    let discovery = client.discover().await.expect("official discovery fixture");
    client
        .verify_imported_credential(&discovery, candidate)
        .await
        .expect_err("invalid metadata must fail before fail-closed verifier")
}

fn candidate(
    base_url: &str,
    scope: &str,
    exported_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
) -> GrokOAuthImportCandidate {
    GrokOAuthImportCandidate::new(
        GrokOAuthImportTokens::new(
            SecretValue::new("fixture-access-token".to_owned()),
            SecretValue::new("fixture-refresh-token".to_owned()),
            SecretValue::new("header.payload.signature".to_owned()),
        ),
        GrokOAuthImportMetadata::new(
            "Bearer".to_owned(),
            provider_xai::OFFICIAL_CLIENT_ID.to_owned(),
            scope.to_owned(),
            base_url.to_owned(),
            exported_at,
            expires_at,
        ),
    )
}

fn oauth_account_document() -> Vec<u8> {
    let exported_at = Utc::now();
    serde_json::to_vec(&serde_json::json!({
        "exported_at": exported_at,
        "accounts": [{
            "name": "grok-primary",
            "platform": "grok",
            "type": "oauth",
            "credentials": {
                "_token_version": 1,
                "access_token": "fixture-access-token",
                "refresh_token": "fixture-refresh-token",
                "id_token": "fixture-id-token",
                "token_type": "Bearer",
                "expires_at": exported_at + Duration::hours(1),
                "email": "owner@example.test",
                "base_url": provider_xai::GROK_CLI_BASE_URL,
                "client_id": provider_xai::OFFICIAL_CLIENT_ID,
                "scope": REQUIRED_SCOPE,
                "sub": "fixture-subject",
                "team_id": "fixture-team"
            },
            "concurrency": 1,
            "priority": 1,
            "rate_multiplier": 1,
            "auto_pause_on_expired": true,
            "extra": {
                "email": "owner@example.test",
                "grok_billing_snapshot": {},
                "grok_usage_snapshot": {}
            }
        }],
        "proxies": []
    }))
    .expect("serialize fixture")
}
