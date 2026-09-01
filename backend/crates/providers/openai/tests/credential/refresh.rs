use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeZone as _, Utc};
use futures::future::BoxFuture;
use gateway_core::account::{
    AccountErrorReason, AccountStateChange, AccountStatus, CredentialState, ProviderAccountId,
    ProviderAccountStore, QuotaAccessChange, QuotaEvidence, QuotaState, QuotaWriteOutcome,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderCredentialState, ProviderCredentialStatePort, ProviderLeaseAcquisition,
    ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshPolicy, ProviderRuntimePolicyPort,
    ProviderSchedulingState, ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use provider_openai::credential::token_client::{
    OpenAiTokenClient, RefreshFailure, TokenClientConfig, TokenPair, TokenRefresher,
};
use provider_openai::credential::{
    CodexCredentialCodec, CodexCredentialRefreshOutcome, CodexCredentialRefreshService,
    ImportCodexOAuthCredential,
};
use secrecy::{ExposeSecret as _, SecretString};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{MemoryAccountStore, profile, secret};

struct SingleUseRefresher {
    calls: AtomicUsize,
    response: Mutex<Option<TokenPair>>,
}

impl SingleUseRefresher {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            response: Mutex::new(Some(TokenPair {
                access_token: Some("refreshed-access-token".to_owned()),
                refresh_token: None,
                id_token: None,
            })),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn with_id_token(id_token: &str) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            response: Mutex::new(Some(TokenPair {
                access_token: Some("refreshed-access-token".to_owned()),
                refresh_token: None,
                id_token: Some(id_token.to_owned()),
            })),
        })
    }

    fn with_access_token(access_token: String) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            response: Mutex::new(Some(TokenPair {
                access_token: Some(access_token),
                refresh_token: None,
                id_token: None,
            })),
        })
    }

    fn without_rotated_tokens() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            response: Mutex::new(Some(TokenPair {
                access_token: None,
                refresh_token: None,
                id_token: None,
            })),
        })
    }
}

#[async_trait]
impl TokenRefresher for SingleUseRefresher {
    async fn refresh(&self, _: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.response
            .lock()
            .expect("refresh response lock")
            .take()
            .ok_or_else(|| RefreshFailure::Transport {
                message: Some("test refresh response is exhausted".to_owned()),
                upstream: None,
            })
    }
}

struct FailingRefresher {
    failure: RefreshFailure,
}

#[async_trait]
impl TokenRefresher for FailingRefresher {
    async fn refresh(&self, _: &str) -> Result<TokenPair, RefreshFailure> {
        Err(self.failure.clone())
    }
}

struct RefreshLeases;

impl ProviderLeasePort for RefreshLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ProviderKind,
        _: &'a [ProviderAccountId],
    ) -> BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async { panic!("scheduled credential refresh does not load scheduling state") })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            match request {
                ProviderLeaseRequest::RefreshCapacity(_) | ProviderLeaseRequest::Refresh(_) => {
                    Ok(ProviderLeaseAcquisition::Acquired(Box::new(())))
                }
                ProviderLeaseRequest::Scheduling(_) => {
                    panic!("scheduled credential refresh must not acquire a scheduling lease")
                }
            }
        })
    }
}

struct RefreshCredentialState;

impl ProviderCredentialStatePort for RefreshCredentialState {
    fn replace(&self, _: ProviderCredentialState) -> BoxFuture<'_, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn read<'a>(
        &'a self,
        _: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_refresh_backoff<'a>(
        &'a self,
        _: &'a ProviderAccountId,
        _: Duration,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async { Ok(1) })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        _: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct MutableRuntimePolicy {
    policy: Mutex<ProviderRefreshPolicy>,
}

impl MutableRuntimePolicy {
    fn new(margin: Duration) -> Arc<Self> {
        Arc::new(Self {
            policy: Mutex::new(refresh_policy(margin)),
        })
    }

    fn set_margin(&self, margin: Duration) {
        *self.policy.lock().expect("runtime policy lock") = refresh_policy(margin);
    }
}

impl ProviderRuntimePolicyPort for MutableRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async { Ok(*self.policy.lock().expect("runtime policy lock")) })
    }
}

fn refresh_policy(margin: Duration) -> ProviderRefreshPolicy {
    ProviderRefreshPolicy::try_new(margin, NonZeroU32::new(1).expect("positive concurrency"))
        .expect("valid refresh policy")
}

fn refresh_service(
    store: &Arc<MemoryAccountStore>,
    refresher: Arc<SingleUseRefresher>,
    runtime_policy: Arc<MutableRuntimePolicy>,
) -> CodexCredentialRefreshService {
    CodexCredentialRefreshService::new(
        store.repository(),
        refresher,
        Arc::new(RefreshLeases),
        Arc::new(RefreshCredentialState),
        runtime_policy,
    )
}

async fn seed_refreshable_account(
    store: &Arc<MemoryAccountStore>,
    account_id: &str,
    access_token_expires_at: SystemTime,
    retry_not_before: Option<SystemTime>,
) {
    let mut verified_account = profile(&format!("chatgpt-{account_id}"));
    verified_account.access_token_expires_at = Some(DateTime::<Utc>::from(access_token_expires_at));
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("access-{account_id}")),
            verified_account,
            next_refresh_at: retry_not_before.map(DateTime::<Utc>::from),
            enabled: true,
        })
        .await;
}

#[tokio::test]
async fn scheduled_refresh_uses_the_current_margin_without_persisting_a_normal_schedule() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(1));
    let refresher = SingleUseRefresher::new();
    let service = refresh_service(&store, Arc::clone(&refresher), Arc::clone(&policy));
    let account_id = "acct_dynamic_margin";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_add(Duration::from_secs(120))
            .expect("test expiry"),
        None,
    )
    .await;

    assert!(
        service
            .refresh_due()
            .await
            .expect("refresh cycle below margin")
            .is_empty()
    );
    assert_eq!(refresher.calls(), 0);
    assert!(
        store
            .account(account_id)
            .expect("seeded account")
            .next_refresh_at()
            .is_none()
    );

    policy.set_margin(Duration::from_secs(5 * 60));
    let outcomes = service
        .refresh_due()
        .await
        .expect("refresh cycle inside margin");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id: refreshed_account_id,
            ..
        }] if refreshed_account_id == account_id
    ));
    assert_eq!(refresher.calls(), 1);
    assert!(
        store
            .account(account_id)
            .expect("refreshed account")
            .next_refresh_at()
            .is_none()
    );
}

#[tokio::test]
async fn scheduled_refresh_rotates_tokens_while_quota_is_exhausted() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let service = refresh_service(&store, SingleUseRefresher::new(), policy);
    let account_id = "acct_exhausted_quota_refresh";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_add(Duration::from_secs(120))
            .expect("test expiry"),
        None,
    )
    .await;
    let account = store.account(account_id).expect("seeded account");
    assert_eq!(
        store
            .apply_quota_access(QuotaAccessChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                state: QuotaState::exhausted(
                    QuotaEvidence::UsageLimitReached,
                    SystemTime::now(),
                    None,
                ),
            })
            .await
            .expect("persist exhausted quota"),
        QuotaWriteOutcome::Updated
    );

    let outcomes = service.refresh_due().await.expect("refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id: refreshed_account_id,
            ..
        }] if refreshed_account_id == account_id
    ));
    let account = store.account(account_id).expect("refreshed account");
    assert!(account.quota().is_exhausted());
    let loaded = store
        .load_credential(account.id(), account.revision())
        .await
        .expect("refreshed credential");
    let runtime = CodexCredentialCodec::decode(&loaded.credential).expect("runtime credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(oauth.access_token.expose_secret(), "refreshed-access-token");
    let expected_refresh_token = format!("rt-access-{account_id}");
    assert_eq!(
        oauth
            .refresh_token
            .as_ref()
            .map(SecretString::expose_secret),
        Some(expected_refresh_token.as_str())
    );
}

#[tokio::test]
async fn scheduled_refresh_persists_the_original_upstream_error_message() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let upstream_message = "Refresh token has already been used.";
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(FailingRefresher {
            failure: RefreshFailure::InvalidGrant {
                message: Some(upstream_message.to_owned()),
                upstream: None,
            },
        }),
        Arc::new(RefreshLeases),
        Arc::new(RefreshCredentialState),
        policy,
    );
    let account_id = "acct_invalid_refresh_token";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_add(Duration::from_secs(120))
            .expect("test expiry"),
        None,
    )
    .await;

    let outcomes = service.refresh_due().await.expect("refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Invalidated {
            account_id: invalidated_account_id,
        }] if invalidated_account_id == account_id
    ));
    assert_eq!(
        store
            .account(account_id)
            .expect("invalidated account")
            .last_error_message(),
        Some(upstream_message)
    );
}

#[tokio::test]
async fn scheduled_refresh_persists_retryable_message_inside_the_two_hour_window() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let upstream_message = "Invalid refresh token.";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {
                "message": upstream_message,
                "type": "invalid_request_error",
                "code": "invalid_refresh_token"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(OpenAiTokenClient::new(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("test HTTP client"),
            TokenClientConfig {
                client_id: "test-public-client".to_owned(),
                token_endpoint: format!("{}/oauth/token", server.uri()),
            },
        )),
        Arc::new(RefreshLeases),
        Arc::new(RefreshCredentialState),
        policy,
    );
    let account_id = "acct_retryable_unauthorized";
    let expires_at = SystemTime::now()
        .checked_sub(Duration::from_secs(30 * 60))
        .expect("expired access token");
    seed_refreshable_account(&store, account_id, expires_at, None).await;

    let outcomes = service.refresh_due().await.expect("refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Transient {
            account_id: deferred_account_id,
        }] if deferred_account_id == account_id
    ));
    let account = store.account(account_id).expect("deferred account");
    assert_eq!(account.credential_state(), CredentialState::Ready);
    assert_eq!(
        account.last_error_reason(),
        Some(AccountErrorReason::AccessTokenExpired)
    );
    assert_eq!(account.last_error_message(), Some(upstream_message));
    let retry_at = account.next_refresh_at().expect("next refresh attempt");
    assert!(retry_at > SystemTime::now());
    assert!(
        retry_at
            <= expires_at
                .checked_add(Duration::from_secs(2 * 60 * 60))
                .expect("recovery deadline")
    );
    let projection = account.status_projection(SystemTime::now(), None);
    assert_eq!(projection.status, AccountStatus::Error);
    assert_eq!(projection.error_message.as_deref(), Some(upstream_message));
}

#[tokio::test]
async fn scheduled_refresh_uses_the_final_message_after_the_two_hour_window() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let upstream_message = "Invalid refresh token.";
    let service = CodexCredentialRefreshService::new(
        store.repository(),
        Arc::new(FailingRefresher {
            failure: RefreshFailure::Transport {
                message: Some(upstream_message.to_owned()),
                upstream: None,
            },
        }),
        Arc::new(RefreshLeases),
        Arc::new(RefreshCredentialState),
        policy,
    );
    let account_id = "acct_exhausted_unauthorized";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_sub(Duration::from_secs(2 * 60 * 60 + 60))
            .expect("expired recovery window"),
        None,
    )
    .await;

    let outcomes = service.refresh_due().await.expect("refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Invalidated {
            account_id: invalidated_account_id,
        }] if invalidated_account_id == account_id
    ));
    let account = store.account(account_id).expect("invalidated account");
    assert_eq!(account.credential_state(), CredentialState::Expired);
    assert_eq!(account.last_error_message(), Some(upstream_message));
}

#[tokio::test]
async fn scheduled_refresh_success_clears_a_deferred_failure_message() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let account_id = "acct_recovered_after_backoff";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .expect("expired access token"),
        None,
    )
    .await;
    let account = store.account(account_id).expect("seeded account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            credential_state: CredentialState::Ready,
            observed_at: SystemTime::now(),
            error_reason: Some(AccountErrorReason::AccessTokenExpired),
            message: Some("Invalid refresh token.".to_owned()),
        })
        .await
        .expect("seed deferred failure");
    let service = refresh_service(&store, SingleUseRefresher::new(), policy);

    let outcomes = service.refresh_due().await.expect("refresh cycle");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id: refreshed_account_id,
            ..
        }] if refreshed_account_id == account_id
    ));
    let account = store.account(account_id).expect("refreshed account");
    assert_eq!(account.credential_state(), CredentialState::Ready);
    assert_eq!(account.last_error_reason(), None);
    assert_eq!(account.last_error_message(), None);
}

#[tokio::test]
async fn scheduled_refresh_persists_a_returned_id_token() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let refresher = SingleUseRefresher::with_id_token("header.rotated-id.signature");
    let service = refresh_service(&store, refresher, policy);
    let account_id = "acct_rotated_id_token";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_add(Duration::from_secs(120))
            .expect("test expiry"),
        None,
    )
    .await;

    service.refresh_due().await.expect("refresh cycle");

    let account = store.account(account_id).expect("refreshed account");
    let loaded = store
        .load_credential(account.id(), account.revision())
        .await
        .expect("refreshed credential");
    let runtime = CodexCredentialCodec::decode(&loaded.credential).expect("runtime credential");
    assert_eq!(
        runtime
            .authentication
            .oauth()
            .and_then(|oauth| oauth.id_token.as_ref())
            .map(SecretString::expose_secret),
        Some("header.rotated-id.signature")
    );
}

#[tokio::test]
async fn scheduled_refresh_uses_the_rotated_access_token_jwt_expiration() {
    let expires_at = Utc
        .timestamp_opt(2_000_000_000, 0)
        .single()
        .expect("valid test timestamp");
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"exp": expires_at.timestamp()}))
            .expect("test JWT payload"),
    );
    let refresher = SingleUseRefresher::with_access_token(format!(
        "unverified-header.{payload}.unverified-signature"
    ));
    let store = Arc::new(MemoryAccountStore::default());
    let service = refresh_service(
        &store,
        refresher,
        MutableRuntimePolicy::new(Duration::from_secs(5 * 60)),
    );
    let account_id = "acct_rotated_access_expiration";
    seed_refreshable_account(
        &store,
        account_id,
        SystemTime::now()
            .checked_add(Duration::from_secs(120))
            .expect("test expiry"),
        None,
    )
    .await;

    service.refresh_due().await.expect("refresh cycle");

    assert_eq!(
        store
            .account(account_id)
            .expect("refreshed account")
            .access_token_expires_at()
            .map(DateTime::<Utc>::from),
        Some(expires_at)
    );
}

#[tokio::test]
async fn scheduled_refresh_preserves_the_stored_token_set_when_rotation_fields_are_omitted() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let refresher = SingleUseRefresher::without_rotated_tokens();
    let service = refresh_service(&store, refresher, policy);
    let account_id = "acct_omitted_token_rotation";
    let expires_at = SystemTime::now()
        .checked_add(Duration::from_secs(120))
        .expect("test expiry");
    seed_refreshable_account(&store, account_id, expires_at, None).await;

    service.refresh_due().await.expect("refresh cycle");

    let account = store.account(account_id).expect("refreshed account");
    assert_eq!(account.access_token_expires_at(), Some(expires_at));
    let loaded = store
        .load_credential(account.id(), account.revision())
        .await
        .expect("refreshed credential");
    let runtime = CodexCredentialCodec::decode(&loaded.credential).expect("runtime credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(
        oauth.access_token.expose_secret(),
        format!("access-{account_id}")
    );
    let expected_refresh_token = format!("rt-access-{account_id}");
    assert_eq!(
        oauth
            .refresh_token
            .as_ref()
            .map(SecretString::expose_secret),
        Some(expected_refresh_token.as_str())
    );
}

#[tokio::test]
async fn scheduled_refresh_respects_retry_not_before_until_it_has_elapsed() {
    let store = Arc::new(MemoryAccountStore::default());
    let policy = MutableRuntimePolicy::new(Duration::from_secs(5 * 60));
    let refresher = SingleUseRefresher::new();
    let service = refresh_service(&store, Arc::clone(&refresher), policy);
    let expires_at = SystemTime::now()
        .checked_add(Duration::from_secs(120))
        .expect("test expiry");
    seed_refreshable_account(
        &store,
        "acct_future_retry",
        expires_at,
        Some(
            SystemTime::now()
                .checked_add(Duration::from_secs(60))
                .expect("future retry"),
        ),
    )
    .await;

    assert!(
        service
            .refresh_due()
            .await
            .expect("refresh cycle before retry")
            .is_empty()
    );
    assert_eq!(refresher.calls(), 0);

    seed_refreshable_account(
        &store,
        "acct_elapsed_retry",
        expires_at,
        Some(
            SystemTime::now()
                .checked_sub(Duration::from_secs(1))
                .expect("past retry"),
        ),
    )
    .await;
    let outcomes = service
        .refresh_due()
        .await
        .expect("refresh cycle after retry");

    assert!(matches!(
        outcomes.as_slice(),
        [CodexCredentialRefreshOutcome::Refreshed {
            account_id,
            ..
        }] if account_id == "acct_elapsed_retry"
    ));
    assert_eq!(refresher.calls(), 1);
}
