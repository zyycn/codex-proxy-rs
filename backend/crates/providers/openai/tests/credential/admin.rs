use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{TimeZone, Utc};
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey as _;
use futures::future::BoxFuture;
use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, LoadedCredential, PlaintextCredential,
    ProviderAccount, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshLeaseRequest,
    ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY, CodexAccountIdentityVerifier,
    CodexAgentIdentityAuthMode, CodexAgentIdentityCredentialData, CodexCredentialAdmin,
    CodexCredentialAdminError, CodexCredentialAdminService, CodexCredentialCodec,
    CodexIdentityExpectation, CodexIdentityVerification, CodexIdentityVerificationError,
    ExportManagedCodexCredential, ImportCodexOAuthCredential, RotateManagedCodexCredential,
};
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

fn unverified_import_access_token(account_id: &str, user_id: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({
            "sub": format!("subject-{user_id}"),
            "exp": Utc::now().timestamp() + 3_600,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_user_id": user_id,
            },
        }))
        .expect("serialize unsigned import JWT"),
    );
    format!("{header}.{payload}.")
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
fn prepare_rotation_rejects_principal_rebinding_without_full_authorization() {
    let mut rebound = profile("chatgpt-acct_rotate");
    rebound.oauth_subject = "different-subject".to_owned();
    rebound.poid = None;
    let error = CodexCredentialAdmin
        .prepare_rotation(RotateManagedCodexCredential {
            current: LoadedCredential {
                account: codex_account("acct_rotate"),
                credential: encoded_credential("chatgpt-acct_rotate", "old-access"),
            },
            secret: secret("new-access"),
            verified_account: rebound,
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        })
        .expect_err("non-authorization rotation cannot replace the principal");

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

fn agent_export_item(id: &str) -> (ExportManagedCodexCredential, String) {
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let private_key = STANDARD.encode(
        signing_key
            .to_pkcs8_der()
            .expect("encode Agent Identity key")
            .as_bytes(),
    );
    let credential =
        CodexCredentialCodec::encode_agent_identity(CodexAgentIdentityCredentialData {
            schema_version: 1,
            auth_mode: CodexAgentIdentityAuthMode::AgentIdentity,
            installation_id: "00000000-0000-4000-8000-000000000007".to_owned(),
            agent_runtime_id: "runtime-export".to_owned(),
            agent_private_key: private_key.clone(),
            task_id: Some("task-export".to_owned()),
            cookies: Vec::new(),
        })
        .expect("encode Agent Identity credential");
    let account = ProviderAccount::new(
        ProviderAccountId::new(id.to_owned()).expect("account id"),
        ProviderKind::new("openai").expect("provider"),
        format!("Agent Identity {id}"),
        "agent-user-export".to_owned(),
        CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY.to_owned(),
        CredentialRevision::new(1).expect("revision"),
        None,
    )
    .with_profile(
        Some("agent-export@example.com".to_owned()),
        Some("agent-account-export".to_owned()),
        Some("pro".to_owned()),
    )
    .with_runtime_state(true, AccountAvailability::Ready)
    .with_refresh_schedule(false, None);
    (
        ExportManagedCodexCredential {
            current: LoadedCredential {
                account,
                credential,
            },
            added_at: Utc
                .with_ymd_and_hms(2026, 7, 18, 2, 47, 1)
                .single()
                .expect("added at"),
            updated_at: Utc
                .with_ymd_and_hms(2026, 7, 19, 3, 0, 0)
                .single()
                .expect("updated at"),
        },
        private_key,
    )
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

#[test]
fn cpr_export_preserves_agent_identity_material_without_oauth_fields() {
    let (item, private_key) = agent_export_item("acct_agent_export");
    let value = CodexCredentialAdmin
        .format_cpr_export(vec![item])
        .expect("Agent Identity export")
        .into_json()
        .expect("serialize Agent Identity export");
    let account = &value["accounts"][0];

    assert_eq!(account["authMode"], "agentIdentity");
    assert_eq!(account["agentRuntimeId"], "runtime-export");
    assert!(
        account["agentPrivateKey"]
            .as_str()
            .is_some_and(|value| value == private_key)
    );
    assert_eq!(account["taskId"], "task-export");
    assert!(account.get("token").is_none());
    assert!(account.get("refreshToken").is_none());
    assert!(account.get("accessTokenExpiresAt").is_none());
}

#[tokio::test]
async fn cpr_export_and_import_support_mixed_oauth_and_agent_identity_accounts() {
    let (agent, _) = agent_export_item("acct_mixed_agent");
    let access_token =
        unverified_import_access_token("chatgpt-acct_mixed_oauth", "user-acct_mixed_oauth");
    let value = CodexCredentialAdmin
        .format_cpr_export(vec![export_item("acct_mixed_oauth", &access_token), agent])
        .expect("mixed export")
        .into_json()
        .expect("serialize mixed export");

    let prepared = import_service(unused_import_refresher())
        .prepare_import_document(value)
        .await
        .expect("mixed CPR import");
    assert_eq!(prepared.accounts().len(), 2);
    assert!(prepared.accounts().iter().any(|account| {
        account.account.authentication_kind() == CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY
    }));
    assert!(
        prepared
            .accounts()
            .iter()
            .any(|account| { account.account.authentication_kind() == "oauth" })
    );
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

struct RejectingManualVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for RejectingManualVerifier {
    async fn verify(
        &self,
        _secret: &provider_openai::credential::CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
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
        refresher.clone(),
        Arc::new(RejectingManualVerifier),
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
async fn manual_refresh_prepares_rotation_without_identity_verification() {
    let (store, refresher, leases, service) =
        manual_refresh_fixture(Ok(refreshed_tokens()), true).await;
    let account_id = ProviderAccountId::new("acct_manual_refresh").expect("account id");
    let current = store
        .load_current_credential(&account_id)
        .await
        .expect("current credential");
    let prepared = service
        .manual_refresh(current)
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
async fn manual_refresh_missing_lease_fails_before_exchange() {
    let (store, unavailable_refresher, _, unavailable_service) =
        manual_refresh_fixture(Ok(refreshed_tokens()), false).await;
    let account_id = ProviderAccountId::new("acct_manual_refresh").expect("account id");
    let current = store
        .load_current_credential(&account_id)
        .await
        .expect("current credential");
    let unavailable = unavailable_service
        .manual_refresh(current)
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

struct RejectingImportVerifier;

#[async_trait]
impl CodexAccountIdentityVerifier for RejectingImportVerifier {
    async fn verify(
        &self,
        _secret: &provider_openai::credential::CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
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
    CodexCredentialAdminService::new(
        refresher,
        Arc::new(RejectingImportVerifier),
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
async fn oauth_import_uses_token_identity_and_source_display_metadata() {
    let refresher = unused_import_refresher();
    let access_token = unverified_import_access_token("chatgpt-cpr", "user-token-cpr");
    let prepared = import_service(refresher.clone())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_cpr_import",
                "email": "cpr@example.com",
                "accountId": "source-account-cpr",
                "userId": "source-user-cpr",
                "label": "CPR account",
                "planType": "pro",
                "token": format!("Bearer {access_token}"),
                "refreshToken": "refresh-cpr",
                "accessTokenExpiresAt": "2100-01-01T00:00:00+00:00",
                "status": "disabled",
                "addedAt": "2026-07-18T10:47:01+08:00",
                "updatedAt": "2026-07-19T11:00:00+08:00",
                "unrelated": { "value": true }
            }]
        }))
        .await
        .expect("CPR import");
    assert_eq!(prepared.accounts().len(), 1);
    let account = &prepared.accounts()[0];
    assert!(account.account.id().as_str().starts_with("acct_"));
    assert!(account.account.enabled());
    assert_eq!(account.account.upstream_account_id(), Some("chatgpt-cpr"));
    assert_eq!(account.account.upstream_user_id(), "user-token-cpr");
    assert_eq!(account.account.name(), "CPR account");
    assert_eq!(account.account.email(), Some("cpr@example.com"));
    assert_eq!(account.account.plan_type(), Some("pro"));
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        access_token
    );
    assert!(refresher.seen.lock().expect("seen tokens").is_empty());
}

#[tokio::test]
async fn refresh_token_only_import_persists_exchanged_tokens_without_identity_verification() {
    let refreshed_access =
        unverified_import_access_token("chatgpt-refreshed-import", "user-refreshed-import");
    let expected_access = refreshed_access.clone();
    let refresher = Arc::new(ManualRefresher {
        outcome: Mutex::new(Some(Ok(TokenPair {
            access_token: refreshed_access,
            refresh_token: Some("rotated-import-refresh".to_owned()),
            expires_in: Duration::from_secs(3_600),
        }))),
        seen: Mutex::new(Vec::new()),
    });

    let prepared = import_service(Arc::clone(&refresher))
        .prepare_import_document(serde_json::json!({
            "refreshToken": "refresh-only"
        }))
        .await
        .expect("refresh-token-only import");

    assert_eq!(prepared.accounts().len(), 1);
    let account = &prepared.accounts()[0];
    assert_eq!(
        account.account.upstream_account_id(),
        Some("chatgpt-refreshed-import")
    );
    assert_eq!(account.account.upstream_user_id(), "user-refreshed-import");
    let runtime = CodexCredentialCodec::decode(&account.credential).expect("credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(oauth.access_token.expose_secret(), expected_access);
    assert_eq!(
        oauth
            .refresh_token
            .as_ref()
            .expect("rotated RT")
            .expose_secret(),
        "rotated-import-refresh"
    );
    assert_eq!(
        refresher
            .seen
            .lock()
            .expect("seen refresh tokens")
            .as_slice(),
        ["refresh-only"]
    );
}

#[tokio::test]
async fn cpr_batch_allows_distinct_users_in_the_same_workspace() {
    let shared_a = unverified_import_access_token("chatgpt-shared", "user-a");
    let shared_b = unverified_import_access_token("chatgpt-shared", "user-b");
    let prepared = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_shared_a",
                "accountId": "chatgpt-shared",
                "userId": "user-a",
                "token": shared_a
            }, {
                "id": "acct_shared_b",
                "accountId": "chatgpt-shared",
                "userId": "user-b",
                "token": shared_b
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

    let duplicate_token = unverified_import_access_token("chatgpt-shared", "user-a");
    let duplicate = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "sourceFormat": "cpr",
            "accounts": [{
                "id": "acct_duplicate_a",
                "accountId": "chatgpt-shared",
                "userId": "user-a",
                "token": duplicate_token.clone()
            }, {
                "id": "acct_duplicate_b",
                "accountId": "chatgpt-shared",
                "userId": "user-a",
                "token": duplicate_token
            }]
        }))
        .await
        .expect_err("the same upstream user and workspace must stay unique");
    assert_eq!(duplicate, CodexCredentialAdminError::InvalidInput);
}

#[tokio::test]
async fn oauth_import_extracts_tokens_and_preserves_source_display_metadata() {
    let bundle_token = unverified_import_access_token("chatgpt-token-bundle", "user-token-bundle");
    let bundle = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "exported_at": "2026-07-03T15:46:38.717Z",
            "proxies": [],
            "accounts": [{
                "name": "bundle@example.com",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": bundle_token,
                    "refresh_token": "refresh-bundle",
                    "chatgpt_account_id": "source-bundle",
                    "chatgpt_user_id": "source-user-bundle",
                    "email": "bundle-source@example.com",
                    "plan_type": "team"
                },
                "concurrency": 3,
                "priority": 50
            }]
        }))
        .await
        .expect("credential bundle import");
    let auth_document_token =
        unverified_import_access_token("chatgpt-token-auth-document", "user-token-auth-document");
    let auth_document = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "accounts": [{
                "type": "openai",
                "access_token": auth_document_token,
                "refresh_token": "refresh-auth-document",
                "chatgpt_account_id": "source-auth-document",
                "chatgpt_user_id": "source-user-auth-document",
                "email": "auth-document@example.com",
                "label": "Auth document",
                "planType": "plus"
            }]
        }))
        .await
        .expect("auth document import");

    let bundle_account = &bundle.accounts()[0].account;
    assert_eq!(bundle.accounts().len(), 1);
    assert_eq!(
        bundle_account.upstream_account_id(),
        Some("chatgpt-token-bundle")
    );
    assert_eq!(bundle_account.upstream_user_id(), "user-token-bundle");
    assert_eq!(bundle_account.name(), "bundle@example.com");
    assert_eq!(bundle_account.email(), Some("bundle-source@example.com"));
    assert_eq!(bundle_account.plan_type(), Some("team"));

    let auth_document_account = &auth_document.accounts()[0].account;
    assert_eq!(auth_document.accounts().len(), 1);
    assert_eq!(
        auth_document_account.upstream_account_id(),
        Some("chatgpt-token-auth-document")
    );
    assert_eq!(
        auth_document_account.upstream_user_id(),
        "user-token-auth-document"
    );
    assert_eq!(auth_document_account.name(), "Auth document");
    assert_eq!(
        auth_document_account.email(),
        Some("auth-document@example.com")
    );
    assert_eq!(auth_document_account.plan_type(), Some("plus"));
    assert!(!format!("{bundle:?} {auth_document:?}").contains("refresh-bundle"));
    assert!(!format!("{bundle:?} {auth_document:?}").contains("refresh-auth-document"));
}

#[tokio::test]
async fn cliproxyapi_codex_auth_file_is_recognized_as_an_openai_auth_document() {
    let access_token = unverified_import_access_token("chatgpt-token-cpa", "user-token-cpa");
    let prepared = import_service(unused_import_refresher())
        .prepare_import_document(serde_json::json!({
            "type": "codex",
            "access_token": access_token,
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
    assert_eq!(account.upstream_account_id(), Some("chatgpt-token-cpa"));
    assert_eq!(account.upstream_user_id(), "user-token-cpa");
    assert_eq!(account.email(), Some("cpa@example.com"));
    assert_eq!(account.authentication_kind(), "oauth");
}
