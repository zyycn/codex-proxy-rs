use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gateway_core::engine::credential::AccountAvailability;
use provider_openai::credential::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use provider_openai::credential::{
    CodexCredentialRefreshOutcome, CodexCredentialRefreshService, CodexRefreshLeaseAcquisition,
    CodexRefreshLeaseCoordinator, CodexRefreshLeaseError, CodexRefreshLeaseRequest,
    CreateCodexCredential,
};
use secrecy::ExposeSecret;

use crate::support::{MemoryAccountStore, instance_id, profile, secret};

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
    requests: Mutex<Vec<CodexRefreshLeaseRequest>>,
}

#[async_trait]
impl CodexRefreshLeaseCoordinator for RefreshLeases {
    async fn try_acquire(
        &self,
        request: &CodexRefreshLeaseRequest,
    ) -> Result<CodexRefreshLeaseAcquisition, CodexRefreshLeaseError> {
        self.requests
            .lock()
            .expect("refresh lease lock")
            .push(request.clone());
        Ok(if self.available {
            CodexRefreshLeaseAcquisition::Acquired(Box::new(()))
        } else {
            CodexRefreshLeaseAcquisition::Unavailable
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
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_refresh".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "refresh".to_owned(),
            secret: secret("old-access"),
            account: profile("chatgpt-acct_refresh"),
            enabled: true,
        })
        .await
        .expect("create account");
    let refresher = Arc::new(Refresher::new(outcome));
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        refresher.clone(),
        Arc::new(RefreshLeases {
            available: lease_available,
            requests: Mutex::new(Vec::new()),
        }),
        Duration::from_secs(60 * 60),
    )
    .expect("refresh service");
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
    let outcomes = service.refresh_due(10).await.expect("refresh due");
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
    assert_eq!(
        refresher.seen.lock().expect("seen tokens lock").as_slice(),
        ["rt-old-access"]
    );
}

#[tokio::test]
async fn invalid_grant_marks_unified_account_expired() {
    let (store, _, service) = setup(Err(RefreshFailure::InvalidGrant), true).await;
    let outcomes = service.refresh_due(10).await.expect("refresh due");
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
async fn upstream_ban_marks_unified_account_banned() {
    let (store, _, service) = setup(Err(RefreshFailure::Banned), true).await;
    service.refresh_due(10).await.expect("refresh due");
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
    let outcomes = service.refresh_due(10).await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Transient { .. }]
    ));
    let account = store.account("acct_refresh").expect("account");
    assert_eq!(account.availability(), AccountAvailability::Cooldown);
    assert!(account.cooldown_until().is_some());
}

#[tokio::test]
async fn ambiguous_refresh_does_not_mutate_tokens_or_account_state() {
    let (store, _, service) = setup(Err(RefreshFailure::Transport), true).await;
    let outcomes = service.refresh_due(10).await.expect("refresh due");
    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Ambiguous { .. }]
    ));
    let account = store.account("acct_refresh").expect("account");
    assert_eq!(account.revision().get(), 1);
    assert_eq!(account.availability(), AccountAvailability::Ready);
}

#[tokio::test]
async fn unavailable_refresh_lease_prevents_duplicate_token_exchange() {
    let (store, refresher, service) = setup(Ok(success_tokens()), false).await;
    let outcomes = service.refresh_due(10).await.expect("refresh due");
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
    assert!(service.refresh_due(0).await.is_err());
    assert!(service.refresh_due(1_001).await.is_err());
}

#[tokio::test]
async fn malformed_account_refresh_does_not_stop_later_accounts() {
    let store = Arc::new(MemoryAccountStore::default());
    for account_id in ["acct_bad", "acct_good"] {
        store
            .repository()
            .create_oauth_credential(CreateCodexCredential {
                account_id: account_id.to_owned(),
                provider_instance_id: instance_id().to_string(),
                name: account_id.to_owned(),
                secret: secret(account_id),
                account: profile(&format!("chatgpt-{account_id}")),
                enabled: true,
            })
            .await
            .expect("create account");
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
        Arc::new(RefreshLeases {
            available: true,
            requests: Mutex::new(Vec::new()),
        }),
        Duration::from_secs(60 * 60),
    )
    .expect("refresh service");

    let outcomes = service
        .refresh_due(10)
        .await
        .expect("isolated refresh cycle");

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
