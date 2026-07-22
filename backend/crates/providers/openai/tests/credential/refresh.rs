use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::engine::credential::AccountAvailability;
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshLeaseRequest,
    ProviderStoreError,
};
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CodexAccountIdentityVerifier, CodexCredentialRefreshOutcome, CodexCredentialRefreshService,
    CodexIdentityExpectation, CodexIdentityVerification, CodexIdentityVerificationError,
    CodexOAuthSecret, CodexSignedIdentity, ImportCodexOAuthCredential, RotateCodexCredential,
};
use secrecy::{ExposeSecret, SecretString};

use crate::support::{MemoryAccountStore, profile, runtime_policy, secret};

struct Refresher {
    outcomes: Mutex<VecDeque<Result<TokenPair, RefreshFailure>>>,
    seen: Mutex<Vec<String>>,
}

impl Refresher {
    fn new(outcome: Result<TokenPair, RefreshFailure>) -> Self {
        Self::scripted([outcome])
    }

    fn scripted(outcomes: impl IntoIterator<Item = Result<TokenPair, RefreshFailure>>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl TokenRefresher for Refresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.seen
            .lock()
            .expect("seen tokens lock")
            .push(refresh_token.to_owned());
        self.outcomes
            .lock()
            .expect("refresh outcomes lock")
            .pop_front()
            .expect("scripted outcome")
    }
}

struct RefreshLeases {
    available: bool,
    requests: Mutex<Vec<ProviderRefreshLeaseRequest>>,
}

struct VerifiedIdentity;

#[async_trait]
impl CodexAccountIdentityVerifier for VerifiedIdentity {
    async fn verify(
        &self,
        _secret: &CodexOAuthSecret,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        let account_id = expectation
            .chatgpt_account_id()
            .ok_or(CodexIdentityVerificationError::Rejected)?;
        Ok(CodexIdentityVerification::Complete(profile(account_id)))
    }

    async fn verify_authorization(
        &self,
        _secret: &CodexOAuthSecret,
        _id_token: &SecretString,
        _expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }
}

struct RejectedIdentity;

#[async_trait]
impl CodexAccountIdentityVerifier for RejectedIdentity {
    async fn verify(
        &self,
        _secret: &CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }

    async fn verify_authorization(
        &self,
        _secret: &CodexOAuthSecret,
        _id_token: &SecretString,
        _expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }
}

struct UnavailableIdentity;

#[async_trait]
impl CodexAccountIdentityVerifier for UnavailableIdentity {
    async fn verify(
        &self,
        _secret: &CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Unavailable)
    }

    async fn verify_authorization(
        &self,
        _secret: &CodexOAuthSecret,
        _id_token: &SecretString,
        _expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Unavailable)
    }
}

struct SignedOnlyIdentity(CodexSignedIdentity);

struct RacingPermanentRefresher {
    store: Arc<MemoryAccountStore>,
}

#[async_trait]
impl TokenRefresher for RacingPermanentRefresher {
    async fn refresh(&self, _: &str) -> Result<TokenPair, RefreshFailure> {
        self.store
            .repository()
            .rotate_oauth_secret(RotateCodexCredential {
                account_id: "acct_refresh".to_owned(),
                expected_credential_revision: 1,
                secret: secret("concurrent-access"),
                verified_account: profile("chatgpt-acct_refresh"),
                next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            })
            .await
            .expect("concurrent credential rotation");
        Err(RefreshFailure::InvalidGrant)
    }
}

#[async_trait]
impl CodexAccountIdentityVerifier for SignedOnlyIdentity {
    async fn verify(
        &self,
        _secret: &CodexOAuthSecret,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Ok(CodexIdentityVerification::SignedOnly(self.0.clone()))
    }

    async fn verify_authorization(
        &self,
        _secret: &CodexOAuthSecret,
        _id_token: &SecretString,
        _expected_nonce: &SecretString,
        _expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        Err(CodexIdentityVerificationError::Rejected)
    }
}

impl ProviderLeasePort for RefreshLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::routing::ProviderKind,
        _: &'a [gateway_core::engine::credential::ProviderAccountId],
    ) -> BoxFuture<
        'a,
        Result<gateway_core::provider_ports::ProviderSchedulingState, ProviderStoreError>,
    > {
        Box::pin(async {
            Ok(gateway_core::provider_ports::ProviderSchedulingState::new(
                Default::default(),
                None,
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
                ProviderLeaseRequest::Refresh(request) => self
                    .requests
                    .lock()
                    .expect("refresh lease lock")
                    .push(request),
                ProviderLeaseRequest::Scheduling(_) => panic!("unexpected scheduling lease"),
            }
            Ok(if self.available {
                ProviderLeaseAcquisition::Acquired(Box::new(()))
            } else {
                ProviderLeaseAcquisition::Busy { retry_after: None }
            })
        })
    }
}

async fn setup(
    outcome: Result<TokenPair, RefreshFailure>,
    lease_available: bool,
) -> (
    Arc<MemoryAccountStore>,
    Arc<Refresher>,
    CodexCredentialRefreshService,
) {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_refresh".to_owned(),
            name: "refresh".to_owned(),
            secret: secret("old-access"),
            verified_account: profile("chatgpt-acct_refresh"),
            next_refresh_at: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
            enabled: true,
        })
        .await;
    let refresher = Arc::new(Refresher::new(outcome));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: lease_available,
            requests: Mutex::new(Vec::new()),
        }),
        runtime_policy(),
    );
    (store, refresher, service)
}

fn success_tokens() -> TokenPair {
    TokenPair {
        access_token: "new-access".to_owned(),
        refresh_token: Some("new-refresh".to_owned()),
        expires_in: Duration::from_secs(2 * 60 * 60),
    }
}

#[tokio::test]
async fn successful_refresh_uses_redis_lease_and_cas_rotates_plaintext_tokens() {
    let (store, refresher, service) = setup(Ok(success_tokens()), true).await;
    let original_account = store.account("acct_refresh").expect("original account");
    let original_installation_id = store
        .repository()
        .load_runtime_credential(&original_account)
        .await
        .expect("original credential")
        .installation_id;
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id,
            credential_revision: 2
        }] if account_id == "acct_refresh"
    ));
    let account = store.account("acct_refresh").expect("rotated account");
    assert_eq!(account.revision().get(), 2);
    let runtime = store
        .repository()
        .load_runtime_credential(&account)
        .await
        .expect("rotated runtime credential");
    assert_eq!(runtime.secret.access_token.expose_secret(), "new-access");
    assert_eq!(runtime.installation_id, original_installation_id);
    assert_eq!(
        refresher.seen.lock().expect("seen tokens lock").as_slice(),
        ["rt-old-access"]
    );
}

#[tokio::test]
async fn refreshed_identity_rejection_revision_fences_account_as_invalid() {
    let (store, _, _) = setup(Ok(success_tokens()), true).await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::new(Ok(success_tokens()))),
        Arc::new(RejectedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        runtime_policy(),
    );

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Invalidated { .. }]
    ));
    assert_eq!(
        store
            .account("acct_refresh")
            .expect("invalid account")
            .availability(),
        AccountAvailability::Invalid
    );
}

#[tokio::test]
async fn unavailable_signature_verification_persists_refresh_backoff() {
    let (store, _, _) = setup(Ok(success_tokens()), true).await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::new(Ok(success_tokens()))),
        Arc::new(UnavailableIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        runtime_policy(),
    );
    let before = SystemTime::now();

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Ambiguous { .. }]
    ));
    let account = store.account("acct_refresh").expect("deferred account");
    assert_eq!(account.revision().get(), 1);
    assert_eq!(account.availability(), AccountAvailability::Invalid);
    assert!(
        account
            .next_refresh_at()
            .is_some_and(|retry| retry < before)
    );
}

#[tokio::test]
async fn unavailable_usage_preserves_rotated_tokens_and_persists_backoff() {
    let store = Arc::new(MemoryAccountStore::default());
    let signed = super::identity::signed_identity_fixture().await;
    let mut account_profile = profile("account-signed");
    account_profile.oauth_subject = signed.oauth_subject().to_owned();
    account_profile.poid = signed.poid().map(str::to_owned);
    account_profile.chatgpt_user_id = "user-signed".to_owned();
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_signed_only".to_owned(),
            name: "signed only".to_owned(),
            secret: secret("old-access"),
            verified_account: account_profile,
            next_refresh_at: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
            enabled: true,
        })
        .await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::new(Ok(success_tokens()))),
        Arc::new(SignedOnlyIdentity(signed)),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        runtime_policy(),
    );
    let before = SystemTime::now();

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    let account = store.account("acct_signed_only").expect("rotated account");
    assert_eq!(account.revision().get(), 2);
    assert!(
        account
            .next_refresh_at()
            .is_some_and(|retry| retry > before)
    );
    let runtime = store
        .repository()
        .load_runtime_credential(&account)
        .await
        .expect("rotated credential");
    assert_eq!(runtime.secret.access_token.expose_secret(), "new-access");
}

#[tokio::test]
async fn invalid_grant_marks_unified_account_expired() {
    let (store, _, service) = setup(Err(RefreshFailure::InvalidGrant), true).await;
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Invalidated { .. }]
    ));
    assert_eq!(
        store
            .account("acct_refresh")
            .expect("account")
            .availability(),
        AccountAvailability::Expired
    );
}

#[tokio::test]
async fn permanent_refresh_failure_cannot_overwrite_a_newer_credential_revision() {
    let (store, _, _) = setup(Ok(success_tokens()), true).await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(RacingPermanentRefresher {
            store: Arc::clone(&store),
        }),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        runtime_policy(),
    );

    assert!(matches!(
        service.refresh_due().await.expect("refresh cycle").as_slice(),
        [CodexCredentialRefreshOutcome::Stale { account_id }] if account_id == "acct_refresh"
    ));
    let account = store.account("acct_refresh").expect("account");
    assert_eq!(account.revision().get(), 2);
    assert_eq!(account.availability(), AccountAvailability::Ready);
}

#[tokio::test]
async fn upstream_ban_marks_unified_account_banned() {
    let (store, _, service) = setup(Err(RefreshFailure::Banned), true).await;
    service.refresh_due().await.expect("refresh due");
    assert_eq!(
        store
            .account("acct_refresh")
            .expect("account")
            .availability(),
        AccountAvailability::Banned
    );
}

#[tokio::test]
async fn proven_pre_send_transport_failure_uses_short_cooldown() {
    let (store, _, service) = setup(Err(RefreshFailure::RetryableTransport), true).await;
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    let account = store.account("acct_refresh").expect("account");
    assert_eq!(account.availability(), AccountAvailability::Ready);
    assert_eq!(account.revision().get(), 2);
    assert!(
        account
            .next_refresh_at()
            .is_some_and(|retry| retry > SystemTime::now())
    );
}

#[tokio::test]
async fn ambiguous_refresh_does_not_mutate_tokens_or_account_state() {
    let (store, refresher, service) = setup(Err(RefreshFailure::Transport), true).await;
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Ambiguous { .. }]
    ));
    let account = store.account("acct_refresh").expect("account");
    assert_eq!(account.revision().get(), 1);
    assert_eq!(account.availability(), AccountAvailability::Invalid);
    assert!(
        service
            .refresh_due()
            .await
            .expect("second refresh cycle")
            .is_empty()
    );
    assert_eq!(refresher.seen.lock().expect("seen tokens lock").len(), 1);
}

#[tokio::test]
async fn unavailable_refresh_lease_prevents_duplicate_token_exchange() {
    let (store, refresher, service) = setup(Ok(success_tokens()), false).await;
    let outcomes = service.refresh_due().await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::LeaseUnavailable { .. }]
    ));
    assert!(refresher.seen.lock().expect("seen tokens lock").is_empty());
    assert_eq!(
        store
            .account("acct_refresh")
            .expect("account")
            .revision()
            .get(),
        1
    );
}

#[tokio::test]
async fn invalid_refresh_batch_limit_fails_before_scanning_accounts() {
    let (_, _, service) = setup(Ok(success_tokens()), true).await;
    assert_eq!(
        service
            .refresh_due()
            .await
            .expect("bounded provider batch")
            .len(),
        1
    );
}

#[tokio::test]
async fn malformed_account_refresh_does_not_stop_later_accounts() {
    let store = Arc::new(MemoryAccountStore::default());
    for account_id in ["acct_bad", "acct_good"] {
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: account_id.to_owned(),
                name: account_id.to_owned(),
                secret: secret(account_id),
                verified_account: profile(&format!("chatgpt-{account_id}")),
                next_refresh_at: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
                enabled: true,
            })
            .await;
    }
    let refresher = Arc::new(Refresher::scripted([
        Ok(TokenPair {
            access_token: "invalid".to_owned(),
            refresh_token: None,
            expires_in: Duration::ZERO,
        }),
        Ok(success_tokens()),
    ]));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher,
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        runtime_policy(),
    );

    let outcomes = service.refresh_due().await.expect("isolated refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [
            CodexCredentialRefreshOutcome::Failed { account_id: failed },
            CodexCredentialRefreshOutcome::Refreshed {
                account_id: refreshed,
                credential_revision: 2,
            },
        ] if failed == "acct_bad" && refreshed == "acct_good"
    ));
    assert_eq!(
        store
            .account("acct_good")
            .expect("good account")
            .revision()
            .get(),
        2
    );
}
