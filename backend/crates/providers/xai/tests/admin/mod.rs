//! 验证 xAI 管理能力、账号清理与后台目录任务的组装和错误处理

mod provider;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_admin::model::accounts::{AccountRecord, AccountUsage};
use gateway_admin::model::observability::{
    CurrencyCost, DesktopReleaseStatus, ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AccountUsagePeriod, CompleteAuthorization, PrepareCredentialImport, PrepareCredentialRefresh,
    PrepareCredentialRotation, ProviderDocument, ProviderExportCredentialInput,
    ProviderQuotaRequest,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::ProviderAdminErrorKind;
use gateway_core::account::{
    AccountRuntimeSignals, CredentialRevision, OpaqueProviderData, ProviderAccount,
    ProviderAccountId, ProviderAccountStore,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::Operation;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingClaimOutcome, OAuthPendingConsumeOutcome,
    OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingReleaseOutcome,
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderCatalogCacheKey,
    ProviderCatalogCachePort, ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope,
    ProviderCredentialState, ProviderCredentialStatePort, ProviderLeaseAcquisition,
    ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshPolicy, ProviderRuntimePolicyPort,
    ProviderScopedCooldown, ProviderStoreError, ProviderStorePorts,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_core::task::{
    WorkerContribution, WorkerCycleContext, WorkerKind, WorkerRunnable, WorkerTaskError,
};
use provider_xai::{
    DiscoveryDocument, GrokAccountSessionSelector, GrokCredentialFailure, GrokCredentialRepository,
    GrokOAuthConfig, GrokSessionBinding, GrokSessionSelector, OfficialGrokEndpointPolicy,
    PendingAuthorization, RedirectUriAllowlist, ReqwestGrokModelCatalogTransport, SecretValue,
    SelectedGrokSession,
};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::support::{
    MemoryCooldownPort, MemoryGrokCatalogCache, MemoryProviderAccountStore, TestSessionAffinity,
    TestSessionExclusions, account_id, create_input, seed_input,
};

mod quota;

async fn run_quota_catalog_cycle(
    bundle: &mut gateway_admin::ports::provider::ProviderBundle,
) -> Result<(), WorkerTaskError> {
    let registration = std::mem::take(&mut bundle.workers)
        .into_iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.kind() == WorkerKind::QuotaCatalogHealth
                    && registration.id.owner() == "xai" =>
            {
                Some(registration)
            }
            WorkerContribution::Registration(_) | WorkerContribution::Disabled { .. } => None,
        })
        .expect("xAI quota/catalog worker");
    let WorkerRunnable::Scheduled { task, .. } = registration.runnable else {
        panic!("xAI quota/catalog worker must be scheduled");
    };
    task.run_cycle(WorkerCycleContext::new(
        registration.id,
        None,
        CancellationToken::new(),
    ))
    .await
}

fn pending_payload(flow_id: &str, owner_ref: &str) -> OpaqueProviderData {
    let config = GrokOAuthConfig::official().expect("official config");
    let discovery = DiscoveryDocument::parse(
        &config,
        include_bytes!("../credential/fixtures/discovery.json"),
    )
    .expect("discovery fixture");
    let redirect = RedirectUriAllowlist::new([provider_xai::OFFICIAL_REDIRECT_URI])
        .expect("redirect allowlist")
        .authorize(provider_xai::OFFICIAL_REDIRECT_URI)
        .expect("official redirect");
    let pending = PendingAuthorization::start(&config, &discovery, redirect, None)
        .expect("pending authorization");
    let server_state = pending
        .into_server_state()
        .expect("server state")
        .expose()
        .to_owned();
    let value = json!({
        "schema_version": 3,
        "flow_id": flow_id,
        "owner_ref": owner_ref,
        "expires_at": (Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
        "server_state": server_state,
        "mutation": {
            "provider_kind": "xai",
            "target": {
                "kind": "create",
                "name": "OAuth account"
            },
            "owner": {
                "kind": "admin_session",
                "admin_user_id": "admin-owner"
            },
            "started_request_id": "request-start"
        }
    });
    OpaqueProviderData::new(value.as_object().expect("pending object").clone())
}

fn admin_session_owner_ref(admin_user_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"admin-session\0");
    digest.update(admin_user_id.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

fn provider_ports() -> ProviderStorePorts {
    provider_ports_with(
        Arc::new(MemoryProviderAccountStore::default()),
        Arc::new(TestOAuthPending::default()),
    )
}

fn provider_ports_with(
    accounts: Arc<MemoryProviderAccountStore>,
    pending: Arc<TestOAuthPending>,
) -> ProviderStorePorts {
    provider_ports_with_catalog(accounts, pending, Arc::new(TestCatalogCache::default()))
}

fn provider_ports_with_catalog(
    accounts: Arc<MemoryProviderAccountStore>,
    pending: Arc<TestOAuthPending>,
    catalog_cache: Arc<TestCatalogCache>,
) -> ProviderStorePorts {
    ProviderStorePorts::new(
        accounts,
        Arc::new(TestLeases),
        Arc::new(TestSessionAffinity::default()),
        Arc::new(TestSessionExclusions),
        catalog_cache,
        Arc::new(TestArtifactProfiles),
        Arc::new(TestCredentialState),
        Arc::new(TestCooldown),
        Arc::new(TestRuntimePolicy),
        pending,
        Arc::new(RecordingDiagnostics::default()),
    )
}

fn account_record(account: &ProviderAccount) -> AccountRecord {
    let now = Utc::now();
    AccountRecord {
        quota_paused: false,
        quota_settings: Default::default(),
        auto_use_quota_credits: false,
        outbound_proxy_id: None,
        outbound_proxy_enabled: true,
        notes: None,
        model_access: Default::default(),
        outbound_proxy: None,
        id: account.id().to_string(),
        provider_kind: account.provider().clone(),
        groups: Vec::new(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        authentication_kind: account.authentication_kind().to_owned(),
        credential_revision: Revision::new(account.revision().get()).expect("revision"),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().map(DateTime::<Utc>::from),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        concurrency_limit: account.concurrency_limit(),
        weight: account.weight(),
        credential_state: account.credential_state(),
        credential_observed_at: now,
        quota: account.quota(),
        last_error_reason: account.last_error_reason(),
        last_error_message: account.last_error_message().map(str::to_owned),
        created_at: now,
        updated_at: now,
    }
}

struct TestLeases;

impl ProviderLeasePort for TestLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::policy::ClientApiKeyId,
        _: &'a gateway_core::routing::ProviderKind,
        accounts: &'a [ProviderAccountId],
        _pool: gateway_core::provider_ports::ProviderConcurrencyPool,
    ) -> BoxFuture<
        'a,
        Result<gateway_core::provider_ports::ProviderSchedulingState, ProviderStoreError>,
    > {
        Box::pin(async move {
            let signals = accounts
                .iter()
                .cloned()
                .map(|account| {
                    (
                        account,
                        AccountRuntimeSignals {
                            in_flight: 0,
                            last_started_at: None,
                            quota_reset_at: None,
                            quota_reset_windows: None,
                            quota_remaining_rank: None,
                            cooldown: None,
                            failure_rate_basis_points: None,
                            first_output_latency_ms: None,
                        },
                    )
                })
                .collect();
            Ok(gateway_core::provider_ports::ProviderSchedulingState::new(
                signals, 0,
            ))
        })
    }

    fn try_acquire(
        &self,
        _request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async { Ok(ProviderLeaseAcquisition::Acquired(Box::new(()))) })
    }
}

struct TestArtifactProfiles;

impl ProviderArtifactProfileCachePort for TestArtifactProfiles {
    fn replace_if_newer(
        &self,
        _profile: ProviderArtifactProfile,
        _ttl: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(true) })
    }

    fn read<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _artifact_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct TestCatalogCache {
    values: Mutex<BTreeMap<String, OpaqueProviderData>>,
}

impl ProviderCatalogCachePort for TestCatalogCache {
    fn replace<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
        catalog: &'a OpaqueProviderData,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.values
                .lock()
                .expect("catalog cache")
                .insert(key.scope().as_str().to_owned(), catalog.clone());
            Ok(())
        })
    }

    fn read<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .values
                .lock()
                .expect("catalog cache")
                .get(key.scope().as_str())
                .cloned())
        })
    }
}

impl TestCatalogCache {
    fn seed(&self, scope: &str, models: impl IntoIterator<Item = &'static str>) {
        let mut document = Map::new();
        document.insert("version".to_owned(), Value::from(1));
        document.insert("scope".to_owned(), Value::String(scope.to_owned()));
        document.insert(
            "observedAt".to_owned(),
            Value::String(Utc::now().to_rfc3339()),
        );
        document.insert(
            "models".to_owned(),
            Value::Array(
                models
                    .into_iter()
                    .map(|model| Value::String(model.to_owned()))
                    .collect(),
            ),
        );
        self.values
            .lock()
            .expect("catalog cache")
            .insert(scope.to_owned(), OpaqueProviderData::new(document));
    }
}

struct TestCredentialState;

impl ProviderCredentialStatePort for TestCredentialState {
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
        _account_id: &'a ProviderAccountId,
        _window: Duration,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async { Ok(1) })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct TestCooldown;

impl ProviderCooldownPort for TestCooldown {
    fn read_many<'a>(
        &'a self,
        account_ids: &'a [ProviderAccountId],
    ) -> BoxFuture<'a, Vec<Result<Option<ProviderCooldown>, ProviderStoreError>>> {
        Box::pin(async move { account_ids.iter().map(|_| Ok(None)).collect() })
    }

    fn read_scoped_many<'a>(
        &'a self,
        account_ids: &'a [ProviderAccountId],
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Vec<Result<Option<ProviderScopedCooldown>, ProviderStoreError>>> {
        Box::pin(async move { account_ids.iter().map(|_| Ok(None)).collect() })
    }

    fn put_if_later(
        &self,
        _cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_scoped_if_later(
        &self,
        _cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_after_success<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: gateway_core::account::CredentialRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct TestRuntimePolicy;

impl ProviderRuntimePolicyPort for TestRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async {
            ProviderRefreshPolicy::try_new(
                Duration::from_secs(300),
                NonZeroU32::new(4).expect("nonzero concurrency"),
            )
        })
    }
}

#[derive(Default)]
struct TestOAuthPending {
    values: Mutex<BTreeMap<PendingKey, PendingValue>>,
}

type PendingKey = (String, String);
type PendingValue = (String, OpaqueProviderData, SystemTime, Option<String>);

impl TestOAuthPending {
    fn insert(&self, flow_id: &str, owner: String, payload: OpaqueProviderData) {
        self.values.lock().expect("OAuth pending").insert(
            ("xai".to_owned(), flow_id.to_owned()),
            (
                owner,
                payload,
                SystemTime::now() + Duration::from_secs(1_800),
                None,
            ),
        );
    }

    fn len(&self) -> usize {
        self.values.lock().expect("OAuth pending").len()
    }
}

impl OAuthPendingFlowPort for TestOAuthPending {
    fn put_if_absent(
        &self,
        flow: NewOAuthPendingFlow,
    ) -> BoxFuture<'_, Result<OAuthPendingPutOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                flow.provider_kind().as_str().to_owned(),
                flow.flow().expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            if values.contains_key(&key) {
                return Ok(OAuthPendingPutOutcome::AlreadyExists);
            }
            values.insert(
                key,
                (
                    flow.owner().expose_to_store().to_owned(),
                    flow.payload().clone(),
                    SystemTime::now() + flow.ttl(),
                    None,
                ),
            );
            Ok(OAuthPendingPutOutcome::Stored)
        })
    }

    fn claim_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
        claim: &'a OAuthPendingBinding,
        _claim_ttl: Duration,
    ) -> BoxFuture<'a, Result<OAuthPendingClaimOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, payload, expires_at, stored_claim)) = values.get_mut(&key)
            else {
                return Ok(OAuthPendingClaimOutcome::NotFound);
            };
            if *expires_at <= SystemTime::now() {
                values.remove(&key);
                return Ok(OAuthPendingClaimOutcome::NotFound);
            }
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingClaimOutcome::OwnerMismatch);
            }
            if stored_claim.is_some() {
                return Ok(OAuthPendingClaimOutcome::InProgress);
            }
            *stored_claim = Some(claim.expose_to_store().to_owned());
            Ok(OAuthPendingClaimOutcome::Claimed(payload.clone()))
        })
    }

    fn release_claim<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
        claim: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingReleaseOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, _, stored_claim)) = values.get_mut(&key) else {
                return Ok(OAuthPendingReleaseOutcome::NotFound);
            };
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingReleaseOutcome::OwnerMismatch);
            }
            if stored_claim.as_deref() != Some(claim.expose_to_store()) {
                return Ok(OAuthPendingReleaseOutcome::ClaimMismatch);
            }
            *stored_claim = None;
            Ok(OAuthPendingReleaseOutcome::Released)
        })
    }

    fn consume_claim<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
        claim: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingConsumeOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, _, stored_claim)) = values.get(&key) else {
                return Ok(OAuthPendingConsumeOutcome::NotFound);
            };
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingConsumeOutcome::OwnerMismatch);
            }
            if stored_claim.as_deref() != Some(claim.expose_to_store()) {
                return Ok(OAuthPendingConsumeOutcome::ClaimMismatch);
            }
            values.remove(&key);
            Ok(OAuthPendingConsumeOutcome::Consumed)
        })
    }
}

mod errors {
    use gateway_admin::ports::provider::ProviderAdminErrorKind as Kind;
    use gateway_core::account::OutboundProxy;

    use super::*;

    #[tokio::test]
    async fn manual_refresh_keeps_expired_and_changed_credentials_distinct_without_network() {
        for expired in [true, false] {
            let store = MemoryProviderAccountStore::shared();
            let input = create_input("refresh_error", "synthetic-subject");
            let mut prepared = crate::support::prepare_input(&input).unwrap();
            if expired {
                // 此处验证已存账号的刷新失败，直接设置持久化期限以免等待真实时间
                let mut credential = prepared.credential.expose_to_provider().clone();
                credential.insert(
                    "refresh_token_expires_at".to_owned(),
                    json!(Utc::now() - chrono::Duration::minutes(1)),
                );
                prepared.credential = gateway_core::account::PlaintextCredential::new(credential);
            }
            store.create_account(prepared).await.unwrap();
            let before = store.account(&input.account_id).unwrap();
            let mut account = account_record(&before);
            if !expired {
                account.upstream_user_id = Some("previous-subject".to_owned());
            }
            let bundle = provider_xai::initialize(provider_ports_with(
                store.clone(),
                Arc::new(TestOAuthPending::default()),
            ))
            .await
            .unwrap();
            let error = bundle
                .admin
                .prepare_refresh(PrepareCredentialRefresh { account })
                .await
                .unwrap_err();
            assert_eq!(
                error.kind(),
                if expired {
                    Kind::Invalid
                } else {
                    Kind::Conflict
                }
            );
            assert_eq!(
                error.public_message(),
                Some(if expired {
                    "刷新令牌已失效，请重新授权"
                } else {
                    "凭据已更新，请刷新账号列表"
                })
            );
            assert_eq!(store.account(&input.account_id).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn manual_refresh_reports_busy_without_calling_the_account_proxy() {
        let store = MemoryProviderAccountStore::shared();
        let input = create_input("refresh_busy", "synthetic-subject");
        seed_input(&store, &input).await.unwrap();
        // 账号使用出站代理时不提前准备直连 discovery；占用失败不得触发任何代理请求
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy =
            OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        store.set_outbound_proxy(&input.account_id, Some(proxy));
        let before = store.account(&input.account_id).unwrap();
        let mut account = account_record(&before);
        account.outbound_proxy = before.outbound_proxy().cloned();
        let ports = ProviderStorePorts::new(
            store.clone(),
            Arc::new(BusyRefreshLeases),
            Arc::new(TestSessionAffinity::default()),
            Arc::new(TestSessionExclusions),
            Arc::new(TestCatalogCache::default()),
            Arc::new(TestArtifactProfiles),
            Arc::new(TestCredentialState),
            Arc::new(TestCooldown),
            Arc::new(TestRuntimePolicy),
            Arc::new(TestOAuthPending::default()),
            Arc::new(RecordingDiagnostics::default()),
        );
        let bundle = provider_xai::initialize(ports).await.unwrap();
        let error = bundle
            .admin
            .prepare_refresh(PrepareCredentialRefresh { account })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Kind::Conflict);
        assert_eq!(error.public_message(), Some("令牌刷新繁忙，请稍后重试"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(store.account(&input.account_id).unwrap(), before);
    }

    #[tokio::test]
    async fn manual_refresh_unavailable_proxy_does_not_expose_proxy_credentials() {
        let store = MemoryProviderAccountStore::shared();
        let input = create_input("refresh_proxy", "synthetic-subject");
        seed_input(&store, &input).await.unwrap();
        let proxy =
            OutboundProxy::parse("http://synthetic-user:proxy-secret-marker@127.0.0.1:1").unwrap();
        store.set_outbound_proxy(&input.account_id, Some(proxy));
        let before = store.account(&input.account_id).unwrap();
        let mut account = account_record(&before);
        account.outbound_proxy = before.outbound_proxy().cloned();
        let bundle = provider_xai::initialize(provider_ports_with(
            store.clone(),
            Arc::new(TestOAuthPending::default()),
        ))
        .await
        .unwrap();
        let error = bundle
            .admin
            .prepare_refresh(PrepareCredentialRefresh { account })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Kind::Unavailable);
        assert_eq!(
            error.public_message(),
            Some("无法连接令牌刷新服务，请检查出站连接")
        );
        assert!(!format!("{error:?} {error}").contains("proxy-secret-marker"));
        assert_eq!(store.account(&input.account_id).unwrap(), before);
    }

    struct BusyRefreshLeases;

    impl ProviderLeasePort for BusyRefreshLeases {
        fn load_state<'a>(
            &'a self,
            _: &'a ClientApiKeyId,
            _: &'a ProviderKind,
            _: &'a [ProviderAccountId],
            _pool: gateway_core::provider_ports::ProviderConcurrencyPool,
        ) -> BoxFuture<
            'a,
            Result<gateway_core::provider_ports::ProviderSchedulingState, ProviderStoreError>,
        > {
            panic!("manual refresh does not use scheduling state")
        }

        fn try_acquire(
            &self,
            request: ProviderLeaseRequest,
        ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
            assert!(matches!(
                request,
                ProviderLeaseRequest::RefreshCapacity(_) | ProviderLeaseRequest::Refresh(_)
            ));
            Box::pin(async { Ok(ProviderLeaseAcquisition::Busy { retry_after: None }) })
        }
    }
}

#[derive(Default)]
struct RecordingDiagnostics(std::sync::Mutex<Vec<gateway_core::diagnostics::OperationalFailure>>);
#[async_trait::async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for RecordingDiagnostics {
    async fn record_failure(
        &self,
        failure: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), gateway_core::error::StoreError> {
        self.0.lock().unwrap().push(failure);
        Ok(())
    }
}
