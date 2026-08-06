use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::engine::credential::{AccountAvailability, ProviderAccountId};
use gateway_core::provider_ports::{
    ProviderCredentialState, ProviderCredentialStatePort, ProviderLeaseAcquisition,
    ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshLeaseRequest, ProviderStoreError,
};
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CodexAccountIdentityVerifier, CodexCredentialRefreshOutcome, CodexCredentialRefreshService,
    CodexIdentityExpectation, CodexIdentityVerification, CodexIdentityVerificationError,
    CodexOAuthSecret, CodexSignedIdentity, ImportCodexOAuthCredential, RotateCodexCredential,
};
use secrecy::{ExposeSecret, SecretString};

use super::{OAuthRecoveryLogCapture, lock_oauth_recovery_log_capture};
use crate::support::{MemoryAccountStore, profile, runtime_policy, secret};

const OAUTH_BACKOFF_ATTEMPTS: u32 = 5;

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

/// 真实累加连续失败计数的测试 double，用于断言退避的指数增长与成功清零。
#[derive(Default)]
struct CountingCredentialState {
    counts: Mutex<HashMap<String, u32>>,
    cleared: Mutex<Vec<String>>,
}

impl CountingCredentialState {
    fn count(&self, account_id: &str) -> u32 {
        self.counts
            .lock()
            .expect("backoff counts lock")
            .get(account_id)
            .copied()
            .unwrap_or(0)
    }

    fn was_cleared(&self, account_id: &str) -> bool {
        self.cleared
            .lock()
            .expect("cleared accounts lock")
            .iter()
            .any(|id| id == account_id)
    }
}

impl ProviderCredentialStatePort for CountingCredentialState {
    fn replace(
        &self,
        _state: ProviderCredentialState,
    ) -> BoxFuture<'_, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_refresh_backoff<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        _window: Duration,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async move {
            let mut counts = self.counts.lock().expect("backoff counts lock");
            let entry = counts.entry(account_id.as_str().to_owned()).or_insert(0);
            *entry += 1;
            Ok(*entry)
        })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.counts
                .lock()
                .expect("backoff counts lock")
                .remove(account_id.as_str());
            self.cleared
                .lock()
                .expect("cleared accounts lock")
                .push(account_id.as_str().to_owned());
            Ok(())
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
        Arc::new(CountingCredentialState::default()),
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
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "new-access"
    );
    assert_eq!(runtime.installation_id, original_installation_id);
    assert_eq!(
        refresher.seen.lock().expect("seen tokens lock").as_slice(),
        ["rt-old-access"]
    );
}

#[tokio::test]
async fn scheduled_refresh_preserves_existing_refresh_token_when_exchange_omits_one() {
    let (store, _, service) = setup(
        Ok(TokenPair {
            access_token: "scheduled-access-without-rotated-refresh".to_owned(),
            refresh_token: None,
            expires_in: Duration::from_secs(3_600),
        }),
        true,
    )
    .await;

    let outcomes = service.refresh_due().await.expect("scheduled refresh");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed { account_id, .. }] if account_id == "acct_refresh"
    ));
    let account = store.account("acct_refresh").expect("rotated account");
    assert_eq!(account.revision().get(), 2);
    let runtime = store
        .repository()
        .load_runtime_credential(&account)
        .await
        .expect("rotated credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "scheduled-access-without-rotated-refresh"
    );
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .refresh_token
            .as_ref()
            .expect("preserved refresh token")
            .expose_secret(),
        "rt-old-access"
    );
}

#[tokio::test]
async fn invalid_refresh_response_is_logged_before_local_rejection() {
    let _capture_lock = lock_oauth_recovery_log_capture().await;
    let (_, _, service) = setup(
        Ok(TokenPair {
            access_token: "rejected-access".to_owned(),
            refresh_token: None,
            expires_in: Duration::ZERO,
        }),
        true,
    )
    .await;
    let recovery_log = OAuthRecoveryLogCapture::default();
    let dispatch = recovery_log.dispatch();
    let guard = tracing::dispatcher::set_default(&dispatch);

    let outcomes = service
        .refresh_due()
        .await
        .expect("refresh cycle completes");
    drop(guard);

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Failed { account_id }] if account_id == "acct_refresh"
    ));
    let record = recovery_log.recovery_record();
    assert_eq!(record["fields"]["operation"], "scheduled_refresh");
    assert_eq!(record["fields"]["account_id"], "acct_refresh");
    assert_eq!(record["fields"]["access_token"], "rejected-access");
    assert_eq!(record["fields"]["refresh_token"], "rt-old-access");
    assert_eq!(record["fields"]["has_refresh_token"], true);
}

#[tokio::test]
async fn refreshed_tokens_persist_when_identity_verification_rejects() {
    let (store, _, _) = setup(Ok(success_tokens()), true).await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::new(Ok(success_tokens()))),
        Arc::new(RejectedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id,
            credential_revision: 2
        }] if account_id == "acct_refresh"
    ));
    let account = store.account("acct_refresh").expect("refreshed account");
    assert_eq!(account.availability(), AccountAvailability::Ready);
    let runtime = store
        .repository()
        .load_runtime_credential(&account)
        .await
        .expect("refreshed credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "new-access"
    );
}

#[tokio::test]
async fn refreshed_tokens_persist_when_identity_verification_is_unavailable() {
    let (store, _, _) = setup(Ok(success_tokens()), true).await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::new(Ok(success_tokens()))),
        Arc::new(UnavailableIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );
    let before = SystemTime::now();

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed { .. }]
    ));
    let account = store.account("acct_refresh").expect("refreshed account");
    assert_eq!(account.revision().get(), 2);
    assert_eq!(account.availability(), AccountAvailability::Ready);
    assert!(
        account
            .next_refresh_at()
            .is_some_and(|retry| retry > before)
    );
}

#[tokio::test]
async fn refreshed_tokens_do_not_require_identity_completion() {
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
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );
    let before = SystemTime::now();

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed { .. }]
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
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .expect("OAuth credential")
            .access_token
            .expose_secret(),
        "new-access"
    );
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
        Arc::new(CountingCredentialState::default()),
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
    assert_eq!(
        store.last_error_message("acct_refresh").as_deref(),
        Some("account_banned")
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
async fn transient_refresh_defers_without_invalidating_account() {
    let (store, refresher, service) = setup(Err(RefreshFailure::Transport), true).await;
    let outcomes = service.refresh_due().await.expect("refresh due");
    // 上游瞬态（含可能已发出的 ambiguous）不再永久失效账号，改为退避重试。
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    let account = store.account("acct_refresh").expect("account");
    assert_eq!(account.revision().get(), 2);
    assert_eq!(account.availability(), AccountAvailability::Ready);
    // 已退避到未来，本轮不再 due，且未重复兑换 token。
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
async fn expired_access_token_respects_persisted_refresh_backoff() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_due_account(&store).await;
    let original = store.account("acct_refresh").expect("seeded account");
    let mut expired_profile = profile("chatgpt-acct_refresh");
    expired_profile.access_token_expires_at =
        Some(chrono::Utc::now() - chrono::Duration::minutes(5));
    store
        .repository()
        .rotate_oauth_secret(RotateCodexCredential {
            account_id: "acct_refresh".to_owned(),
            expected_credential_revision: original.revision().get(),
            secret: secret("old-access"),
            verified_account: expired_profile,
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::hours(12)),
        })
        .await
        .expect("seed expired access token with deferred refresh");
    let refresher = Arc::new(Refresher::new(Ok(success_tokens())));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(outcomes.is_empty());
    assert!(refresher.seen.lock().expect("seen tokens lock").is_empty());
    assert!(
        store
            .account("acct_refresh")
            .expect("deferred account")
            .next_refresh_at()
            .is_some_and(|next_refresh_at| next_refresh_at > SystemTime::now())
    );
}

#[tokio::test]
async fn expired_access_token_refreshes_when_persisted_refresh_is_due() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_due_account(&store).await;
    let original = store.account("acct_refresh").expect("seeded account");
    let mut expired_profile = profile("chatgpt-acct_refresh");
    expired_profile.access_token_expires_at =
        Some(chrono::Utc::now() - chrono::Duration::minutes(5));
    store
        .repository()
        .rotate_oauth_secret(RotateCodexCredential {
            account_id: "acct_refresh".to_owned(),
            expected_credential_revision: original.revision().get(),
            secret: secret("old-access"),
            verified_account: expired_profile,
            next_refresh_at: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        })
        .await
        .expect("seed expired access token with deferred refresh");
    let deferred = store.account("acct_refresh").expect("deferred account");
    store
        .repository()
        .apply_state(&deferred, AccountAvailability::Ready, SystemTime::now())
        .await
        .expect("mark account ready");
    let refresher = Arc::new(Refresher::new(Ok(success_tokens())));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id,
            credential_revision: 3,
        }] if account_id == "acct_refresh"
    ));
    assert_eq!(
        refresher.seen.lock().expect("seen tokens lock").as_slice(),
        ["rt-old-access"]
    );
    let refreshed = store.account("acct_refresh").expect("refreshed account");
    assert_eq!(refreshed.availability(), AccountAvailability::Ready);
    assert!(
        refreshed
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at > SystemTime::now())
    );
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
        Arc::new(CountingCredentialState::default()),
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

async fn seed_due_account(store: &MemoryAccountStore) {
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
}

async fn force_due_account(store: &Arc<MemoryAccountStore>) {
    let account = store.account("acct_refresh").expect("account to reset");
    store
        .repository()
        .defer_refresh(&account, SystemTime::now() - Duration::from_secs(1))
        .await
        .expect("reset next_refresh_at to the past");
}

#[tokio::test]
async fn refresh_backoff_grows_exponentially_across_attempts() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_due_account(&store).await;
    let credential_state = Arc::new(CountingCredentialState::default());
    let state_port: Arc<dyn ProviderCredentialStatePort> = credential_state.clone();
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::scripted([
            Err(RefreshFailure::Transport),
            Err(RefreshFailure::Transport),
        ])),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        state_port,
        runtime_policy(),
    );

    let before_first = SystemTime::now();
    let first = service.refresh_due().await.expect("first refresh cycle");
    assert!(matches!(
        first.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    let first_account = store
        .account("acct_refresh")
        .expect("account after first defer");
    let first_delay = first_account
        .next_refresh_at()
        .expect("first retry scheduled")
        .duration_since(before_first)
        .expect("first delay is in the future");
    assert_eq!(credential_state.count("acct_refresh"), 1);

    // 已退避到未来的账号真实调度不会再次到期，这里手动复位以触发第二次退避。
    store
        .repository()
        .defer_refresh(&first_account, SystemTime::now() - Duration::from_secs(1))
        .await
        .expect("reset next_refresh_at to the past");

    let before_second = SystemTime::now();
    let second = service.refresh_due().await.expect("second refresh cycle");
    assert!(matches!(
        second.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    let second_delay = store
        .account("acct_refresh")
        .expect("account after second defer")
        .next_refresh_at()
        .expect("second retry scheduled")
        .duration_since(before_second)
        .expect("second delay is in the future");
    assert_eq!(credential_state.count("acct_refresh"), 2);

    // base=5s、factor=3：第二次（attempt=2）应比第一次（attempt=1）显著更久。
    assert!(
        second_delay > first_delay * 2,
        "second backoff {second_delay:?} should grow well beyond first {first_delay:?}"
    );
}

#[tokio::test]
async fn exhausted_oauth_backoff_continues_after_access_token_expiry() {
    let store = Arc::new(MemoryAccountStore::default());
    let mut expired_profile = profile("chatgpt-acct_refresh");
    expired_profile.access_token_expires_at =
        Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_refresh".to_owned(),
            name: "refresh".to_owned(),
            secret: secret("old-access"),
            verified_account: expired_profile,
            next_refresh_at: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
            enabled: true,
        })
        .await;
    let credential_state = Arc::new(CountingCredentialState::default());
    let state_port: Arc<dyn ProviderCredentialStatePort> = credential_state.clone();
    let refresher = Arc::new(Refresher::scripted([
        Err(RefreshFailure::Transport),
        Err(RefreshFailure::Transport),
        Err(RefreshFailure::Transport),
        Err(RefreshFailure::Transport),
        Err(RefreshFailure::Transport),
        Err(RefreshFailure::Transport),
    ]));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        state_port,
        runtime_policy(),
    );

    for _ in 0..OAUTH_BACKOFF_ATTEMPTS {
        service.refresh_due().await.expect("backoff attempt");
        force_due_account(&store).await;
    }

    let observed_at = SystemTime::now();
    service.refresh_due().await.expect("recovery attempt");
    let recovery_delay = store
        .account("acct_refresh")
        .expect("account after recovery defer")
        .next_refresh_at()
        .expect("recovery scheduled")
        .duration_since(observed_at)
        .expect("recovery is in the future");
    assert!(
        (Duration::from_secs(8 * 60)..=Duration::from_secs(12 * 60 + 1)).contains(&recovery_delay),
        "recovery delay should be about ten minutes, got {recovery_delay:?}"
    );
    assert_eq!(
        credential_state.count("acct_refresh"),
        OAUTH_BACKOFF_ATTEMPTS + 1
    );
    assert_eq!(refresher.seen.lock().expect("seen tokens lock").len(), 6);
}

#[tokio::test]
async fn oauth_backoff_never_passes_an_unexpired_access_token_expiry() {
    let store = Arc::new(MemoryAccountStore::default());
    let mut account_profile = profile("chatgpt-acct_refresh");
    account_profile.access_token_expires_at =
        Some(chrono::Utc::now() + chrono::Duration::seconds(30));
    let access_token_expires_at: SystemTime = account_profile
        .access_token_expires_at
        .expect("access expiry")
        .into();
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_refresh".to_owned(),
            name: "refresh".to_owned(),
            secret: secret("old-access"),
            verified_account: account_profile,
            next_refresh_at: Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
            enabled: true,
        })
        .await;
    let credential_state = Arc::new(CountingCredentialState::default());
    let state_port: Arc<dyn ProviderCredentialStatePort> = credential_state.clone();
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::scripted([
            Err(RefreshFailure::Transport),
            Err(RefreshFailure::Transport),
            Err(RefreshFailure::Transport),
            Err(RefreshFailure::Transport),
            Err(RefreshFailure::Transport),
            Err(RefreshFailure::Transport),
        ])),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        state_port,
        runtime_policy(),
    );

    for attempt in 0..=OAUTH_BACKOFF_ATTEMPTS {
        service.refresh_due().await.expect("backoff attempt");
        if attempt < OAUTH_BACKOFF_ATTEMPTS {
            force_due_account(&store).await;
        }
    }

    assert_eq!(
        store
            .account("acct_refresh")
            .expect("account after capped retry")
            .next_refresh_at(),
        Some(access_token_expires_at)
    );
    assert_eq!(
        credential_state.count("acct_refresh"),
        OAUTH_BACKOFF_ATTEMPTS + 1
    );
}

#[tokio::test]
async fn recovery_window_exhaustion_marks_account_expired_without_token_exchange() {
    let store = Arc::new(MemoryAccountStore::default());
    let mut expired_profile = profile("chatgpt-acct_refresh");
    expired_profile.access_token_expires_at =
        Some(chrono::Utc::now() - chrono::Duration::hours(25));
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_refresh".to_owned(),
            name: "refresh".to_owned(),
            secret: secret("old-access"),
            verified_account: expired_profile,
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::hours(12)),
            enabled: true,
        })
        .await;
    let refresher = Arc::new(Refresher::new(Ok(success_tokens())));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        Arc::new(CountingCredentialState::default()),
        runtime_policy(),
    );

    let outcomes = service.refresh_due().await.expect("refresh due");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Invalidated { account_id }] if account_id == "acct_refresh"
    ));
    assert!(refresher.seen.lock().expect("seen tokens lock").is_empty());
    assert_eq!(
        store
            .account("acct_refresh")
            .expect("expired account")
            .availability(),
        AccountAvailability::Expired
    );
}

#[tokio::test]
async fn successful_refresh_clears_backoff_counter() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_due_account(&store).await;
    let credential_state = Arc::new(CountingCredentialState::default());
    let state_port: Arc<dyn ProviderCredentialStatePort> = credential_state.clone();
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(Refresher::scripted([
            Err(RefreshFailure::Transport),
            Ok(success_tokens()),
        ])),
        Arc::new(VerifiedIdentity),
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        state_port,
        runtime_policy(),
    );

    // 第一次瞬态失败 → 计数累加到 1。
    let first = service.refresh_due().await.expect("first refresh cycle");
    assert!(matches!(
        first.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    assert_eq!(credential_state.count("acct_refresh"), 1);

    // 复位到期后成功刷新，退避计数被清零。
    let deferred = store.account("acct_refresh").expect("deferred account");
    store
        .repository()
        .defer_refresh(&deferred, SystemTime::now() - Duration::from_secs(1))
        .await
        .expect("reset next_refresh_at to the past");

    let second = service.refresh_due().await.expect("second refresh cycle");
    assert!(matches!(
        second.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed { .. }]
    ));
    assert!(credential_state.was_cleared("acct_refresh"));
    assert_eq!(credential_state.count("acct_refresh"), 0);
}
