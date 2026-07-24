use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::future::BoxFuture;
use gateway_core::engine::credential::{
    CredentialRevision, LoadedCredential, PlaintextCredential, ProviderAccountId,
};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshLeaseRequest,
    ProviderStoreError,
};
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CodexAccountIdentityService, CodexAccountIdentityVerifier, CodexCredentialAdmin,
    CodexCredentialAdminError, CodexCredentialAdminService, CodexCredentialCodec,
    CodexIdentityExpectation, CodexIdentityVerification, CodexIdentityVerificationError,
    CodexJwtIdentityVerifier, ExportManagedCodexCredential, ImportCodexOAuthCredential,
    ReqwestCodexAuthenticatedAccountSource, ReqwestOpenAiJwksSource, RotateManagedCodexCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use secrecy::{ExposeSecret, SecretString};

use crate::support::{MemoryAccountStore, codex_account, profile, runtime_policy, secret};

fn import(id: &str, token: &str) -> ImportCodexOAuthCredential {
    ImportCodexOAuthCredential {
        account_id: id.to_owned(),
        name: format!("name-{id}"),
        secret: secret(token),
        verified_account: profile(&format!("chatgpt-{id}")),
        next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        enabled: true,
    }
}

fn encoded_credential(account_id: &str, token: &str) -> PlaintextCredential {
    CodexCredentialCodec::encode_new(&secret(token), &profile(account_id), Vec::new())
        .expect("encode current credential")
}

#[test]
fn prepare_import_returns_core_account_with_plaintext_json() {
    let prepared = CodexCredentialAdmin
        .prepare_import(import("acct_import", "access-import"))
        .expect("prepare import");
    assert_eq!(prepared.account.id().as_str(), "acct_import");
    assert_eq!(prepared.account.provider().as_str(), "openai");
    assert_eq!(
        prepared
            .credential
            .expose_to_provider()
            .get("access_token")
            .and_then(serde_json::Value::as_str),
        Some("access-import")
    );
}

#[test]
fn prepare_rotation_preserves_provider_owned_cookie_data() {
    let account = codex_account("acct_rotate");
    let mut credential = encoded_credential("chatgpt-acct_rotate", "old-access");
    credential
        .expose_to_provider()
        .get("access_token")
        .expect("access token");
    let mut data = CodexCredentialCodec::decode_complete(&credential).expect("decode data");
    data.oauth_mut().expect("OAuth data").oauth_client_id = Some("oauth-client".to_owned());
    credential = CodexCredentialCodec::encode_complete(data).expect("encode complete data");
    let prepared = CodexCredentialAdmin
        .prepare_rotation(RotateManagedCodexCredential {
            current: LoadedCredential {
                account,
                credential,
            },
            secret: secret("new-access"),
            verified_account: profile("chatgpt-acct_rotate"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        })
        .expect("prepare rotation");
    let decoded = CodexCredentialCodec::decode(prepared.credential.credential())
        .expect("decode prepared credential");
    assert_eq!(
        decoded
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "new-access"
    );
    assert_eq!(decoded.oauth_client_id.as_deref(), Some("oauth-client"));
}

#[test]
fn prepare_rotation_rejects_account_rebinding() {
    let error = CodexCredentialAdmin
        .prepare_rotation(RotateManagedCodexCredential {
            current: LoadedCredential {
                account: codex_account("acct_rotate"),
                credential: encoded_credential("chatgpt-acct_rotate", "old-access"),
            },
            secret: secret("new-access"),
            verified_account: profile("chatgpt-other"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        })
        .expect_err("identity rebinding must fail");
    assert_eq!(error, CodexCredentialAdminError::IdentityMismatch);
}

#[test]
fn prepared_commands_debug_never_prints_tokens() {
    let prepared = CodexCredentialAdmin
        .prepare_import(import("acct_debug", "debug-secret"))
        .expect("prepare import");
    assert!(!format!("{prepared:?}").contains("debug-secret"));
    let _: PlaintextCredential = prepared.credential;
}

fn export_item(id: &str, token: &str) -> ExportManagedCodexCredential {
    ExportManagedCodexCredential {
        current: LoadedCredential {
            account: codex_account(id),
            credential: encoded_credential(&format!("chatgpt-{id}"), token),
        },
        added_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 2, 47, 1)
            .single()
            .expect("added at"),
        updated_at: Utc
            .with_ymd_and_hms(2026, 7, 19, 3, 0, 0)
            .single()
            .expect("updated at"),
    }
}

#[test]
fn cpr_export_matches_the_canonical_real_document_shape() {
    let document = CodexCredentialAdmin
        .format_cpr_export(vec![export_item("acct_export", "export-secret")])
        .expect("format export");
    assert_eq!(document.len(), 1);
    let value = document.into_json().expect("serialize export");
    assert_eq!(value["sourceFormat"], "cpr");
    let account = &value["accounts"][0];
    let mut keys = account
        .as_object()
        .expect("account object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "accessTokenExpiresAt",
            "accountId",
            "addedAt",
            "email",
            "id",
            "label",
            "planType",
            "refreshToken",
            "status",
            "token",
            "updatedAt",
            "userId",
        ]
    );
    assert_eq!(account["id"], "acct_export");
    assert_eq!(account["token"], "export-secret");
    assert_eq!(account["refreshToken"], "rt-export-secret");
    assert_eq!(account["status"], "active");
    assert!(
        account["addedAt"]
            .as_str()
            .is_some_and(|value| value.ends_with("+08:00"))
    );
    assert!(
        account["accessTokenExpiresAt"]
            .as_str()
            .is_some_and(|value| value.ends_with("+00:00"))
    );
}

#[test]
fn cpr_export_batch_validation_and_debug_are_secret_safe() {
    assert_eq!(
        CodexCredentialAdmin
            .format_cpr_export(Vec::new())
            .expect_err("empty export"),
        CodexCredentialAdminError::InvalidInput
    );
    let duplicate = CodexCredentialAdmin
        .format_cpr_export(vec![
            export_item("acct_duplicate_export", "first-secret"),
            export_item("acct_duplicate_export", "second-secret"),
        ])
        .expect_err("duplicate export account");
    assert_eq!(duplicate, CodexCredentialAdminError::InvalidInput);

    let document = CodexCredentialAdmin
        .format_cpr_export(vec![export_item("acct_debug_export", "never-print-me")])
        .expect("export document");
    let debug = format!("{document:?}");
    assert!(!debug.contains("never-print-me"));
    assert!(!debug.contains("rt-never-print-me"));
    assert!(debug.contains("account_count: 1"));
}

pub(super) struct ManualRefresher {
    outcome: Mutex<Option<Result<TokenPair, RefreshFailure>>>,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl TokenRefresher for ManualRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.seen
            .lock()
            .expect("seen refresh tokens")
            .push(refresh_token.to_owned());
        self.outcome
            .lock()
            .expect("refresh outcome")
            .take()
            .expect("one refresh outcome")
    }
}

struct ManualVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for ManualVerifier {
    async fn verify(
        &self,
        secret: &provider_openai::credential::CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        if secret.access_token.expose_secret() != "refreshed-access" {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        Ok(CodexIdentityVerification::Complete(profile(
            "chatgpt-acct_manual_refresh",
        )))
    }

    async fn verify_authorization(
        &self,
        _secret: &provider_openai::credential::CodexOAuthSecret,
        _id_token: &SecretString,
        _expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }
}

struct DropCountGuard(Arc<AtomicUsize>);

impl Drop for DropCountGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct ManualLeases {
    requests: Mutex<Vec<ProviderRefreshLeaseRequest>>,
    drops: Arc<AtomicUsize>,
    available: bool,
}

impl ProviderLeasePort for ManualLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::routing::ProviderKind,
        _: &'a [ProviderAccountId],
    ) -> BoxFuture<
        'a,
        Result<gateway_core::provider_ports::ProviderSchedulingState, ProviderStoreError>,
    > {
        Box::pin(async {
            Ok(gateway_core::provider_ports::ProviderSchedulingState::new(
                Default::default(),
                0,
            ))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            match request {
                ProviderLeaseRequest::RefreshCapacity(_) => {}
                ProviderLeaseRequest::Refresh(request) => {
                    self.requests.lock().expect("lease requests").push(request);
                }
                ProviderLeaseRequest::Scheduling(_) => panic!("unexpected scheduling lease"),
            }
            Ok(if self.available {
                ProviderLeaseAcquisition::Acquired(Box::new(DropCountGuard(Arc::clone(
                    &self.drops,
                ))))
            } else {
                ProviderLeaseAcquisition::Busy { retry_after: None }
            })
        })
    }
}

async fn manual_refresh_fixture(
    outcome: Result<TokenPair, RefreshFailure>,
    available: bool,
) -> (
    Arc<MemoryAccountStore>,
    Arc<ManualRefresher>,
    Arc<ManualLeases>,
    CodexCredentialAdminService,
) {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_manual_refresh".to_owned(),
            name: "manual refresh".to_owned(),
            secret: secret("old-access"),
            verified_account: profile("chatgpt-acct_manual_refresh"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let refresher = Arc::new(ManualRefresher {
        outcome: Mutex::new(Some(outcome)),
        seen: Mutex::new(Vec::new()),
    });
    let leases = Arc::new(ManualLeases {
        requests: Mutex::new(Vec::new()),
        drops: Arc::new(AtomicUsize::new(0)),
        available,
    });
    let service = CodexCredentialAdminService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(ManualVerifier),
        leases.clone(),
        runtime_policy(),
    );
    (store, refresher, leases, service)
}

fn refreshed_tokens() -> TokenPair {
    TokenPair {
        access_token: "refreshed-access".to_owned(),
        refresh_token: Some("rotated-refresh".to_owned()),
        expires_in: Duration::from_secs(3_600),
    }
}

#[tokio::test]
async fn manual_refresh_prepares_revision_fenced_rotation_without_store_mutation() {
    let (store, refresher, leases, service) =
        manual_refresh_fixture(Ok(refreshed_tokens()), true).await;
    let account_id = ProviderAccountId::new("acct_manual_refresh").expect("account id");
    let prepared = service
        .manual_refresh(
            account_id.clone(),
            CredentialRevision::new(1).expect("revision"),
        )
        .await
        .expect("manual refresh");

    assert_eq!(prepared.credential.account_id(), &account_id);
    assert_eq!(prepared.credential.expected_revision().get(), 1);
    let runtime = CodexCredentialCodec::decode(prepared.credential.credential())
        .expect("prepared credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "refreshed-access"
    );
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .refresh_token
            .as_ref()
            .expect("rotated refresh")
            .expose_secret(),
        "rotated-refresh"
    );
    assert_eq!(
        store
            .account("acct_manual_refresh")
            .expect("account")
            .revision()
            .get(),
        1
    );
    assert_eq!(
        refresher.seen.lock().expect("seen tokens").as_slice(),
        ["rt-old-access"]
    );
    assert_eq!(leases.drops.load(Ordering::SeqCst), 0);
    drop(prepared);
    assert_eq!(leases.drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn manual_refresh_stale_revision_and_missing_lease_fail_before_exchange() {
    let (_, stale_refresher, _, stale_service) =
        manual_refresh_fixture(Ok(refreshed_tokens()), true).await;
    let account_id = ProviderAccountId::new("acct_manual_refresh").expect("account id");
    let stale = stale_service
        .manual_refresh(
            account_id.clone(),
            CredentialRevision::new(2).expect("revision"),
        )
        .await
        .expect_err("stale revision");
    assert_eq!(stale, CodexCredentialAdminError::RevisionConflict);
    assert!(stale_refresher.seen.lock().expect("seen tokens").is_empty());

    let (_, unavailable_refresher, _, unavailable_service) =
        manual_refresh_fixture(Ok(refreshed_tokens()), false).await;
    let unavailable = unavailable_service
        .manual_refresh(account_id, CredentialRevision::new(1).expect("revision"))
        .await
        .expect_err("lease unavailable");
    assert_eq!(
        unavailable,
        CodexCredentialAdminError::RefreshLeaseUnavailable
    );
    assert!(
        unavailable_refresher
            .seen
            .lock()
            .expect("seen tokens")
            .is_empty()
    );
}

struct ImportVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for ImportVerifier {
    async fn verify(
        &self,
        secret: &provider_openai::credential::CodexOAuthSecret,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        let suffix = secret
            .access_token
            .expose_secret()
            .strip_prefix("token-")
            .ok_or(CodexIdentityVerificationError::Rejected)?;
        let mut profile = profile(&format!("chatgpt-{suffix}"));
        if let Some(user) = suffix.strip_prefix("shared-") {
            profile.chatgpt_account_id = "chatgpt-shared".to_owned();
            profile.chatgpt_user_id = format!("user-{user}");
        }
        if expectation
            .chatgpt_account_id()
            .is_some_and(|expected| expected != profile.chatgpt_account_id)
            || expectation
                .chatgpt_user_id()
                .is_some_and(|expected| expected != profile.chatgpt_user_id)
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        Ok(CodexIdentityVerification::Complete(profile))
    }

    async fn verify_authorization(
        &self,
        _secret: &provider_openai::credential::CodexOAuthSecret,
        _id_token: &SecretString,
        _expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }
}

pub(super) fn import_service(
    refresher: Arc<ManualRefresher>,
) -> provider_openai::credential::CodexCredentialAdminService {
    let store = Arc::new(MemoryAccountStore::default());
    CodexCredentialAdminService::new(
        store.repository(),
        refresher,
        Arc::new(ImportVerifier),
        Arc::new(ManualLeases {
            requests: Mutex::new(Vec::new()),
            drops: Arc::new(AtomicUsize::new(0)),
            available: true,
        }),
        runtime_policy(),
    )
}

pub(super) fn unused_import_refresher() -> Arc<ManualRefresher> {
    Arc::new(ManualRefresher {
        outcome: Mutex::new(Some(Err(RefreshFailure::InvalidGrant))),
        seen: Mutex::new(Vec::new()),
    })
}

#[tokio::test]
async fn formal_cpr_import_is_strict_and_uses_the_single_core_write_shape() {
    let refresher = unused_import_refresher();
    let prepared = import_service(refresher.clone())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_cpr_import",
                "email": "cpr@example.com",
                "accountId": "chatgpt-cpr",
                "userId": "user-chatgpt-cpr",
                "label": "CPR account",
                "planType": "pro",
                "token": "Bearer token-cpr",
                "refreshToken": "refresh-cpr",
                "accessTokenExpiresAt": "2100-01-01T00:00:00+00:00",
                "status": "disabled",
                "addedAt": "2026-07-18T10:47:01+08:00",
                "updatedAt": "2026-07-19T11:00:00+08:00"
            }]
        }))
        .await
        .expect("CPR import");
    assert_eq!(prepared.accounts().len(), 1);
    let account = &prepared.accounts()[0];
    assert_eq!(account.account.id().as_str(), "acct_cpr_import");
    assert!(!account.account.enabled());
    assert_eq!(account.account.upstream_account_id(), Some("chatgpt-cpr"));
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "token-cpr"
    );
    assert!(refresher.seen.lock().expect("seen tokens").is_empty());

    let error = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{"token": "token-cpr", "unexpected": true}]
        }))
        .await
        .expect_err("unknown CPR account key");
    assert_eq!(error, CodexCredentialAdminError::InvalidInput);
}

#[tokio::test]
async fn cpr_import_uses_verified_token_identity_instead_of_stale_export_projections() {
    let error = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_verified_identity",
                "accountId": "user-chatgpt-cpr",
                "userId": "user-chatgpt-cpr",
                "token": "token-cpr"
            }]
        }))
        .await
        .expect_err("stale document identity must not override authenticated identity");

    assert_eq!(error, CodexCredentialAdminError::IdentityRejected);
}

#[tokio::test]
async fn cpr_batch_allows_distinct_users_in_the_same_workspace() {
    let prepared = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_shared_a",
                "accountId": "chatgpt-shared",
                "userId": "user-a",
                "token": "token-shared-a"
            }, {
                "id": "acct_shared_b",
                "accountId": "chatgpt-shared",
                "userId": "user-b",
                "token": "token-shared-b"
            }]
        }))
        .await
        .expect("distinct users sharing one workspace are separate credentials");

    assert_eq!(prepared.accounts().len(), 2);
    assert!(
        prepared
            .accounts()
            .iter()
            .all(|account| { account.account.upstream_account_id() == Some("chatgpt-shared") })
    );

    let duplicate = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_duplicate_a",
                "accountId": "chatgpt-shared",
                "userId": "user-a",
                "token": "token-shared-a"
            }, {
                "id": "acct_duplicate_b",
                "accountId": "chatgpt-shared",
                "userId": "user-a",
                "token": "token-shared-a"
            }]
        }))
        .await
        .expect_err("the same upstream user and workspace must stay unique");
    assert_eq!(duplicate, CodexCredentialAdminError::InvalidInput);
}

#[tokio::test]
async fn credential_bundle_and_auth_document_normalize_to_the_same_core_accounts() {
    let bundle = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "exported_at": "2026-07-03T15:46:38.717Z",
            "proxies": [],
            "accounts": [{
                "name": "bundle@example.com",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "at": "token-bundle",
                    "refresh_token": "refresh-bundle",
                    "chatgpt_account_id": "chatgpt-bundle",
                    "chatgpt_user_id": "user-chatgpt-bundle"
                },
                "concurrency": 3,
                "priority": 50
            }]
        }))
        .await
        .expect("credential bundle import");
    let auth_document = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "accounts": [{
                "type": "openai",
                "access_token": "token-auth-document",
                "refresh_token": "refresh-auth-document",
                "chatgpt_account_id": "chatgpt-auth-document",
                "chatgpt_user_id": "user-chatgpt-auth-document",
                "email": "auth-document@example.com",
                "label": "Auth document"
            }]
        }))
        .await
        .expect("auth document import");

    for (prepared, expected) in [
        (&bundle, "chatgpt-bundle"),
        (&auth_document, "chatgpt-auth-document"),
    ] {
        assert_eq!(prepared.accounts().len(), 1);
        assert_eq!(
            prepared.accounts()[0].account.upstream_account_id(),
            Some(expected)
        );
        assert!(
            prepared.accounts()[0]
                .account
                .id()
                .as_str()
                .starts_with("acct_")
        );
    }
    assert!(!format!("{bundle:?} {auth_document:?}").contains("refresh-bundle"));
    assert!(!format!("{bundle:?} {auth_document:?}").contains("refresh-auth-document"));
}

#[tokio::test]
async fn cliproxyapi_codex_auth_file_is_recognized_as_an_openai_auth_document() {
    let prepared = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "type": "codex",
            "access_token": "token-cpa",
            "refresh_token": "refresh-cpa",
            "id_token": "id-cpa",
            "account_id": "chatgpt-cpa",
            "email": "cpa@example.com",
            "expired": "2100-01-01T00:00:00Z"
        }))
        .await
        .expect("CLIProxyAPI Codex auth file import");

    assert_eq!(prepared.accounts().len(), 1);
    let account = &prepared.accounts()[0].account;
    assert_eq!(account.upstream_account_id(), Some("chatgpt-cpa"));
    assert_eq!(account.email(), Some("chatgpt-cpa@example.com"));
    assert_eq!(account.authentication_kind(), "oauth");
}

#[tokio::test]
#[ignore = "requires CODEX_REAL_ACCOUNT_FIXTURE and live official OpenAI OAuth/JWKS"]
async fn real_cpr_fixture_import_contract() {
    let fixture = std::env::var("CODEX_REAL_ACCOUNT_FIXTURE")
        .expect("CODEX_REAL_ACCOUNT_FIXTURE must point to a CPR JSON document");
    let payload = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read(fixture).expect("read CPR fixture"),
    )
    .expect("parse CPR fixture");
    let store = Arc::new(MemoryAccountStore::default());
    let refresher = Arc::new(
        provider_openai::credential::token_client::official_openai_token_client()
            .expect("official token client"),
    );
    let signed = Arc::new(CodexJwtIdentityVerifier::new(Box::new(
        ReqwestOpenAiJwksSource::new().expect("official JWKS source"),
    )));
    let profile = CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "xterm".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    });
    let accounts = Arc::new(
        ReqwestCodexAuthenticatedAccountSource::new(profile)
            .expect("official authenticated account source"),
    );
    let verifier = Arc::new(CodexAccountIdentityService::new(signed, accounts));
    let service = CodexCredentialAdminService::new(
        store.repository(),
        refresher,
        verifier,
        Arc::new(ManualLeases {
            requests: Mutex::new(Vec::new()),
            drops: Arc::new(AtomicUsize::new(0)),
            available: true,
        }),
        runtime_policy(),
    );
    let prepared = service
        .prepare_import_document(payload)
        .await
        .expect("real CPR import");

    assert!(!prepared.accounts().is_empty());
    for account in prepared.accounts() {
        assert_eq!(account.account.provider().as_str(), "openai");
        assert!(account.account.id().as_str().starts_with("acct_"));
        assert!(account.account.upstream_account_id().is_some());
        assert!(!account.account.upstream_user_id().is_empty());
        let runtime =
            CodexCredentialCodec::decode(&account.credential).expect("new credential schema");
        let principal = runtime.principal.as_ref().expect("OAuth principal");
        assert!(principal.poid.is_some());
        assert_ne!(
            principal.poid.as_deref(),
            account.account.upstream_account_id()
        );
        assert_eq!(
            uuid::Uuid::parse_str(&runtime.installation_id)
                .expect("random installation UUID")
                .get_version_num(),
            4
        );
    }
}
