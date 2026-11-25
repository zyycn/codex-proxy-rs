//! 共享内存账号、目录、租约与原生请求上下文

mod ports;
pub(crate) mod profile;

use crate::{
    credential::catalog::CatalogService,
    credential::{CredentialService, GoogleOAuth},
    profile::ReleaseService,
    provider::AntigravityProvider,
};
use futures::future::BoxFuture;
use gateway_core::{
    account::{
        AccountCatalogObservation, AccountSelectionPolicy, CredentialRevision, CredentialState,
        LoadedCredential, OpaqueProviderData, PlaintextCredential, ProviderAccount,
        ProviderAccountId, QuotaState, RotationStrategy,
    },
    engine::{
        AccountAttemptContext, AttemptContext, ModelRequestId, RequestAttemptContext,
        provider::ProviderRequest,
    },
    lifecycle::CancellationToken,
    operation::{GenerateRequest, Operation, ProtocolPayload},
    policy::ClientApiKeyId,
    provider_ports::{ProviderLeaseGuard, ProviderStoreError, ProviderStorePorts},
    routing::{
        ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
        ProviderModel, PublicModelId, RoutingContext, RuntimeAccount, RuntimeAccountDirectory,
        RuntimeSnapshot, UpstreamModelId,
    },
    settings::SettingsValues,
};
use gateway_protocol::genai::GENAI_PROTOCOL;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

const PRIMARY_ACCOUNT: &str = "acct_fixture";

pub(crate) struct MemoryPorts {
    pub(crate) bindings: Mutex<
        BTreeMap<
            gateway_core::provider_ports::ProviderSessionAffinityKey,
            (
                gateway_core::provider_ports::ProviderSessionBinding,
                Duration,
            ),
        >,
    >,
    pub(crate) reject_next_claim: std::sync::atomic::AtomicBool,
    pub(crate) claims: AtomicUsize,
    pub(crate) scheduling_reads: AtomicUsize,
    pub(crate) assignment_cursors: Mutex<BTreeMap<ClientApiKeyId, u64>>,
    pub(crate) admission_barrier: Mutex<Option<Arc<tokio::sync::Barrier>>>,
    pub(crate) request_profile: Mutex<Option<OpaqueProviderData>>,
    accounts: BTreeMap<ProviderAccountId, MemoryAccount>,
    refresh_leases: Arc<AtomicUsize>,
    pub(crate) cooldowns: Mutex<Vec<gateway_core::provider_ports::ProviderScopedCooldown>>,
    pub(crate) expired: std::sync::atomic::AtomicBool,
}

pub(crate) struct MemoryAccount {
    pub(crate) loaded: Mutex<LoadedCredential>,
    pub(crate) catalog: Mutex<Option<(AccountCatalogObservation, bytes::Bytes)>>,
    active_leases: Arc<AtomicUsize>,
    pub(crate) quota: Mutex<Option<gateway_core::account::QuotaObservation>>,
}

impl MemoryPorts {
    pub(crate) fn primary(&self) -> &MemoryAccount {
        &self.accounts[&ProviderAccountId::new(PRIMARY_ACCOUNT).unwrap()]
    }

    pub(crate) fn active_leases(&self) -> usize {
        self.refresh_leases.load(Ordering::SeqCst)
            + self
                .accounts
                .values()
                .map(|account| account.active_leases.load(Ordering::SeqCst))
                .sum::<usize>()
    }

    pub(crate) fn account_scope(&self) -> Arc<FrozenAccountScope> {
        account_scope(self.accounts.keys().cloned())
    }
}

struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ProviderLeaseGuard for Lease {
    fn release(self: Box<Self>) -> BoxFuture<'static, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            drop(self);
            Ok(())
        })
    }
}

pub(crate) struct Fixture {
    pub(crate) provider: Arc<AntigravityProvider>,
    pub(crate) catalog: Arc<CatalogService>,
    pub(crate) store: Arc<MemoryPorts>,
    pub(crate) feedback: Arc<gateway_core::account::AccountFeedbackStats>,
    pub(crate) quota: Arc<crate::credential::quota::QuotaService>,
}

impl Fixture {
    pub(crate) fn new(origin: &str) -> Self {
        Self::with_accounts(origin, &["fixture"])
    }

    pub(crate) fn with_two_accounts(origin: &str) -> Self {
        Self::with_accounts(origin, &["fixture", "standby"])
    }

    fn with_accounts(origin: &str, names: &[&str]) -> Self {
        let kind = ProviderKind::new("antigravity").unwrap();
        let oauth = Arc::new(GoogleOAuth::for_provider().unwrap());
        let accounts = names
            .iter()
            .map(|name| {
                let id = ProviderAccountId::new(format!("acct_{name}")).unwrap();
                let subject = format!("{name}-subject");
                let account = ProviderAccount::new(
                    id.clone(),
                    kind.clone(),
                    "Fixture".into(),
                    Some(subject.clone()),
                    "oauth".into(),
                    CredentialRevision::new(1).unwrap(),
                    Some(SystemTime::now() + Duration::from_secs(3600)),
                )
                .with_account_facts(
                    true,
                    CredentialState::Ready,
                    QuotaState::unknown(),
                    None,
                    None,
                );
                let observation = AccountCatalogObservation {
                    account_id: id.clone(),
                    revision: account.revision(),
                    plan: None,
                    etag: Some(oauth.registration()),
                    observed_at: SystemTime::now(),
                };
                let material = json!({
                    "registration": oauth.registration(),
                    "subject": subject,
                    "access_token": format!("{name}-access"),
                    "refresh_token": format!("{name}-refresh"),
                    "scopes": ["fixture-resource"]
                });
                let catalog = json!({
                    "project": format!("{name}-project"),
                    "models": {"fixture-model": {"displayName": "Fixture Model"}}
                });
                (
                    id,
                    MemoryAccount {
                        loaded: Mutex::new(LoadedCredential {
                            account,
                            credential: PlaintextCredential::new(
                                material.as_object().unwrap().clone(),
                            ),
                        }),
                        catalog: Mutex::new(Some((
                            observation,
                            bytes::Bytes::from(serde_json::to_vec(&catalog).unwrap()),
                        ))),
                        active_leases: Arc::new(AtomicUsize::new(0)),
                        quota: Mutex::new(None),
                    },
                )
            })
            .collect();
        let store = Arc::new(MemoryPorts {
            bindings: Mutex::new(BTreeMap::new()),
            reject_next_claim: Default::default(),
            claims: AtomicUsize::new(0),
            scheduling_reads: AtomicUsize::new(0),
            assignment_cursors: Mutex::new(BTreeMap::new()),
            admission_barrier: Mutex::new(None),
            request_profile: Mutex::new(None),
            accounts,
            refresh_leases: Arc::new(AtomicUsize::new(0)),
            cooldowns: Mutex::new(Vec::new()),
            expired: Default::default(),
        });
        let ports = ProviderStorePorts::new(
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
        );
        let release = Arc::new(ReleaseService::new(crate::support::profile::cache()).unwrap());
        let credentials = Arc::new(CredentialService::new(
            kind.clone(),
            &ports,
            oauth.clone(),
            release.clone(),
        ));
        let origin: url::Url = origin.parse().unwrap();
        let catalog = Arc::new(CatalogService::new(
            kind.clone(),
            store.clone(),
            credentials.clone(),
            oauth.registration(),
            origin.clone(),
        ));
        let quota = Arc::new(crate::credential::quota::QuotaService::new(
            kind.clone(),
            store.clone(),
            credentials.clone(),
            catalog.clone(),
            origin.clone(),
        ));
        let provider = Arc::new(AntigravityProvider::new(
            kind,
            &ports,
            credentials,
            catalog.clone(),
            quota.clone(),
            release,
            origin,
        ));
        Self {
            provider,
            catalog,
            store,
            feedback: ports.account_feedback(),
            quota,
        }
    }
}

fn account_scope(accounts: impl IntoIterator<Item = ProviderAccountId>) -> Arc<FrozenAccountScope> {
    let kind = ProviderKind::new("antigravity").unwrap();
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            accounts
                .into_iter()
                .map(|id| (id, RuntimeAccount::new(kind.clone(), BTreeSet::new())))
                .collect(),
        )),
        ClientRoutingScope::all_accounts(),
    ))
}

fn primary_scope() -> Arc<FrozenAccountScope> {
    account_scope([ProviderAccountId::new(PRIMARY_ACCOUNT).unwrap()])
}

pub(crate) fn request(stream: bool) -> ProviderRequest {
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            GENAI_PROTOCOL,
            json!({"contents":[{"parts":[{"text":"hello"}]}]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_context(json!({"stream":stream}).as_object().unwrap().clone()),
    ));
    let kind = ProviderKind::new("antigravity").unwrap();
    let model = ProviderModel::new(
        kind.clone(),
        UpstreamModelId::new("fixture-model").unwrap(),
        ModelCapabilities::new(BTreeSet::from([operation.kind()]), None)
            .with_upstream_feature_validation(),
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        SettingsValues::new(2, "smart", Default::default(), None, None),
        vec![kind],
        vec![model],
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new("fixture-model").unwrap(),
            &operation,
            primary_scope(),
            &RoutingContext::default(),
        )
        .unwrap();
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

pub(crate) fn context(
    profile: OpaqueProviderData,
    cancellation: CancellationToken,
) -> AttemptContext {
    context_with_request(
        RequestAttemptContext::new(
            ModelRequestId::new("req_fixture").unwrap(),
            ClientApiKeyId::new("key_fixture").unwrap(),
        )
        .with_request_profile(Some(profile)),
        cancellation,
    )
}

pub(crate) fn context_with_request(
    request: RequestAttemptContext,
    cancellation: CancellationToken,
) -> AttemptContext {
    context_with_policy(
        request,
        AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::MIN),
        cancellation,
    )
}

pub(crate) fn context_with_policy(
    request: RequestAttemptContext,
    policy: AccountSelectionPolicy,
    cancellation: CancellationToken,
) -> AttemptContext {
    context_with_scope(request, policy, cancellation, primary_scope())
}

pub(crate) fn context_with_scope(
    request: RequestAttemptContext,
    policy: AccountSelectionPolicy,
    cancellation: CancellationToken,
    scope: Arc<FrozenAccountScope>,
) -> AttemptContext {
    AttemptContext::new(
        request,
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(30),
        policy,
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        cancellation,
    )
}

pub(crate) fn account_record() -> gateway_admin::model::accounts::AccountRecord {
    gateway_admin::model::accounts::AccountRecord {
        quota_paused: false,
        quota_settings: Default::default(),
        auto_use_quota_credits: false,
        id: "acct_fixture".into(),
        provider_kind: ProviderKind::new("antigravity").unwrap(),
        groups: vec![],
        name: "Local name".into(),
        notes: None,
        email: None,
        upstream_user_id: Some("fixture-subject".into()),
        upstream_account_id: None,
        plan_type: None,
        authentication_kind: "oauth".into(),
        credential_revision: gateway_admin::model::Revision::new(1).unwrap(),
        has_refresh_token: true,
        access_token_expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        next_refresh_at: None,
        enabled: true,
        concurrency_limit: None,
        weight: Default::default(),
        model_access: Default::default(),
        outbound_proxy_id: None,
        outbound_proxy: None,
        outbound_proxy_enabled: true,
        credential_state: CredentialState::Ready,
        credential_observed_at: chrono::Utc::now(),
        quota: QuotaState::unknown(),
        last_error_reason: None,
        last_error_message: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}
