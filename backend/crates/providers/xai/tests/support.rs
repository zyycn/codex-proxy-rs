use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use gateway_core::engine::credential::{
    AccountErrorReason, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision, CredentialState, LoadedCredential, NewProviderAccount, PlaintextCredential,
    ProviderAccount, ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
    QuotaAccessChange, QuotaObservation, QuotaState, QuotaWriteOutcome,
};
use gateway_core::error::{StoreError, StoreErrorKind};
use gateway_core::provider_ports::{
    ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderScopedCooldown, ProviderSessionAffinityKey,
    ProviderSessionAffinityPort, ProviderSessionExclusionPort, ProviderSessionExclusions,
    ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use provider_xai::{
    CreateGrokCredential, GROK_BILLING_URL, GrokAccountProfile, GrokBillingTransport,
    GrokCatalogCacheError, GrokCatalogScope, GrokCredentialAdmin, GrokCredentialCatalogCache,
    GrokCredentialCatalogService, GrokCredentialQuotaService, GrokCredentialRepository,
    GrokCredentialRepositoryError, GrokEndpointPolicy, GrokModelCatalogTransport, GrokOAuthSecret,
    GrokPlanCatalog, GrokReqwestTransportBuildError, SecretValue, XaiConfig, XaiWireProfileState,
};
use reqwest::Client;
use reqwest::redirect::Policy;
use url::{Host, Url};

pub fn xai_config() -> XaiConfig {
    serde_json::from_value(serde_json::json!({
        "wire_profile": {
            "client_identifier": "grok-shell",
            "client_version": "0.2.106",
            "client_mode": "headless",
            "target_os": "linux",
            "target_arch": "x86_64",
            "verified_at": "2026-07-21T00:00:00+08:00"
        }
    }))
    .expect("valid xAI test config")
}

pub fn xai_wire_profile() -> XaiWireProfileState {
    xai_config().wire_profile_state()
}

pub fn grok_catalog_service(
    repository: GrokCredentialRepository,
    transport: Arc<dyn GrokModelCatalogTransport>,
    cache: Arc<dyn GrokCredentialCatalogCache>,
) -> GrokCredentialCatalogService {
    GrokCredentialCatalogService::new(repository, transport, cache, xai_wire_profile())
}

pub fn grok_quota_service(
    repository: GrokCredentialRepository,
    transport: Arc<dyn GrokBillingTransport>,
) -> GrokCredentialQuotaService {
    GrokCredentialQuotaService::new(repository, transport, xai_wire_profile())
}

#[derive(Debug, Clone, Copy)]
pub struct LoopbackGrokEndpointPolicy {
    address: IpAddr,
    port: u16,
}

impl LoopbackGrokEndpointPolicy {
    pub fn for_origin(origin: &Url) -> Self {
        assert_eq!(origin.scheme(), "http", "test origin must use HTTP");
        assert!(
            origin.username().is_empty(),
            "test origin must not contain credentials"
        );
        assert!(
            origin.password().is_none(),
            "test origin must not contain credentials"
        );
        assert!(
            origin.query().is_none(),
            "test origin must not contain a query"
        );
        assert!(
            origin.fragment().is_none(),
            "test origin must not contain a fragment"
        );
        let address = match origin.host() {
            Some(Host::Ipv4(address)) if address.is_loopback() => IpAddr::V4(address),
            Some(Host::Ipv6(address)) if address.is_loopback() => IpAddr::V6(address),
            _ => panic!("test origin must use a numeric loopback address"),
        };
        Self {
            address,
            port: origin.port().expect("test origin must contain a port"),
        }
    }

    fn accepts_origin(self, url: &Url) -> bool {
        url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.host().and_then(|host| match host {
                Host::Ipv4(address) => Some(IpAddr::V4(address)),
                Host::Ipv6(address) => Some(IpAddr::V6(address)),
                Host::Domain(_) => None,
            }) == Some(self.address)
            && url.port() == Some(self.port)
            && url.fragment().is_none()
    }

    fn build_client(timeout: Option<Duration>) -> Result<Client, GrokReqwestTransportBuildError> {
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .https_only(false)
            .tcp_nodelay(true);
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        builder
            .build()
            .map_err(|_| GrokReqwestTransportBuildError::ClientInitialization)
    }
}

impl GrokEndpointPolicy for LoopbackGrokEndpointPolicy {
    fn build_oauth_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError> {
        Self::build_client(timeout)
    }

    fn build_inference_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError> {
        Self::build_client(timeout)
    }

    fn validate_oauth(&self, url: &Url) -> bool {
        self.accepts_origin(url)
    }

    fn validate_inference(&self, url: &Url) -> bool {
        self.accepts_origin(url) && url.path() == "/v1/responses" && url.query().is_none()
    }

    fn validate_model_catalog(&self, url: &Url) -> bool {
        self.accepts_origin(url) && url.path() == "/v1/models" && url.query().is_none()
    }

    fn route_billing(&self, url: &Url) -> Option<Url> {
        (url.as_str() == GROK_BILLING_URL).then(|| {
            let mut endpoint = self.origin();
            endpoint.set_path("/v1/billing");
            endpoint.set_query(Some("format=credits"));
            endpoint
        })
    }

    fn validate_jwks(&self, url: &Url) -> bool {
        self.accepts_origin(url) && url.path() == "/.well-known/jwks.json" && url.query().is_none()
    }

    fn validate_userinfo(&self, url: &Url) -> bool {
        self.accepts_origin(url) && url.path() == "/oauth2/userinfo" && url.query().is_none()
    }
}

impl LoopbackGrokEndpointPolicy {
    fn origin(self) -> Url {
        let origin = match self.address {
            IpAddr::V4(address) => format!("http://{address}:{}", self.port),
            IpAddr::V6(address) => format!("http://[{address}]:{}", self.port),
        };
        Url::parse(&origin).expect("validated loopback origin")
    }
}

pub fn loopback_endpoint_policy(origin: &Url) -> Arc<dyn GrokEndpointPolicy> {
    Arc::new(LoopbackGrokEndpointPolicy::for_origin(origin))
}

#[derive(Clone)]
struct StoredAccount {
    account: ProviderAccount,
    credential: PlaintextCredential,
    quota: Option<QuotaObservation>,
}

#[derive(Default)]
pub struct MemoryProviderAccountStore {
    accounts: Mutex<BTreeMap<ProviderAccountId, StoredAccount>>,
    quota_reads: AtomicUsize,
    fail_provider_listing: AtomicBool,
}

impl MemoryProviderAccountStore {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn account(&self, id: &ProviderAccountId) -> Option<ProviderAccount> {
        lock(&self.accounts)
            .get(id)
            .map(|stored| stored.account.clone())
    }

    pub fn last_error_message(&self, id: &ProviderAccountId) -> Option<String> {
        lock(&self.accounts)
            .get(id)
            .and_then(|stored| stored.account.last_error_message().map(str::to_owned))
    }

    pub fn credential(&self, id: &ProviderAccountId) -> Option<PlaintextCredential> {
        lock(&self.accounts)
            .get(id)
            .map(|stored| stored.credential.clone())
    }

    pub fn len(&self) -> usize {
        lock(&self.accounts).len()
    }

    pub fn quota_reads(&self) -> usize {
        self.quota_reads.load(Ordering::SeqCst)
    }

    pub fn fail_provider_listing(&self) {
        self.fail_provider_listing.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ProviderAccountStore for MemoryProviderAccountStore {
    async fn create_account(&self, account: NewProviderAccount) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        if accounts.contains_key(account.account.id()) {
            return Err(conflict());
        }
        accounts.insert(
            account.account.id().clone(),
            StoredAccount {
                account: account.account,
                credential: account.credential,
                quota: None,
            },
        );
        Ok(())
    }

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self.account(account))
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(lock(&self.accounts)
            .values()
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        if self.fail_provider_listing.load(Ordering::SeqCst) {
            return Err(invalid());
        }
        Ok(lock(&self.accounts)
            .values()
            .filter(|stored| stored.account.provider() == provider)
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError> {
        let loaded = self.load_current_credential(account).await?;
        if loaded.account.revision() != expected_revision {
            return Err(conflict());
        }
        Ok(loaded)
    }

    async fn load_current_credential(
        &self,
        account: &ProviderAccountId,
    ) -> Result<LoadedCredential, StoreError> {
        let accounts = lock(&self.accounts);
        let stored = accounts.get(account).ok_or_else(invalid)?;
        Ok(LoadedCredential {
            account: stored.account.clone(),
            credential: stored.credential.clone(),
        })
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError> {
        let (
            account_id,
            expected_revision,
            profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
        ) = update.into_parts();
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&account_id).ok_or_else(invalid)?;
        if stored.account.revision() != expected_revision {
            return Ok(CredentialCasOutcome::Conflict);
        }
        let next = expected_revision.next().map_err(|_| invalid())?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: next,
                access_token_expires_at,
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
                enabled: stored.account.enabled(),
                has_refresh_token,
                next_refresh_at,
                name: profile.name,
                email: profile.email,
                plan_type: profile.plan_type,
            },
        );
        stored.credential = credential;
        stored.quota = None;
        Ok(CredentialCasOutcome::Updated(next))
    }

    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError> {
        self.quota_reads.fetch_add(1, Ordering::SeqCst);
        let stored = lock(&self.accounts);
        Ok(accounts
            .iter()
            .filter_map(|account| stored.get(account)?.quota.clone())
            .collect())
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts
            .get_mut(&observation.account_id)
            .ok_or_else(invalid)?;
        if stored.account.revision() != observation.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        let quota = observation.state;
        stored.quota = Some(observation);
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement::preserving(&stored.account).with_quota(quota),
        );
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_quota_access(
        &self,
        change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&change.account_id).ok_or_else(invalid)?;
        if stored.account.revision() != change.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement::preserving(&stored.account).with_quota(change.state),
        );
        if let Some(observation) = &mut stored.quota {
            observation.state = change.state;
        }
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&change.account_id).ok_or_else(invalid)?;
        if stored.account.revision() != change.expected_revision {
            return Err(conflict());
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                credential_state: change.credential_state,
                quota: stored.account.quota(),
                last_error_reason: change.error_reason,
                last_error_message: change.message.clone(),
                enabled: stored.account.enabled(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: stored.account.name().to_owned(),
                email: stored.account.email().map(str::to_owned),
                plan_type: stored.account.plan_type().map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&update.account_id).ok_or_else(invalid)?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
                enabled: stored.account.enabled(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: update.name,
                email: update.email,
                plan_type: update.plan_type,
            },
        );
        Ok(())
    }

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(account).ok_or_else(invalid)?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
                enabled,
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: stored.account.name().to_owned(),
                email: stored.account.email().map(str::to_owned),
                plan_type: stored.account.plan_type().map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), StoreError> {
        lock(&self.accounts).remove(account).ok_or_else(invalid)?;
        Ok(())
    }
}

struct AccountReplacement {
    revision: CredentialRevision,
    access_token_expires_at: Option<SystemTime>,
    credential_state: CredentialState,
    quota: QuotaState,
    last_error_reason: Option<AccountErrorReason>,
    last_error_message: Option<String>,
    enabled: bool,
    has_refresh_token: bool,
    next_refresh_at: Option<SystemTime>,
    name: String,
    email: Option<String>,
    plan_type: Option<String>,
}

impl AccountReplacement {
    fn preserving(account: &ProviderAccount) -> Self {
        Self {
            revision: account.revision(),
            access_token_expires_at: account.access_token_expires_at(),
            credential_state: account.credential_state(),
            quota: account.quota(),
            last_error_reason: account.last_error_reason(),
            last_error_message: account.last_error_message().map(str::to_owned),
            enabled: account.enabled(),
            has_refresh_token: account.has_refresh_token(),
            next_refresh_at: account.next_refresh_at(),
            name: account.name().to_owned(),
            email: account.email().map(str::to_owned),
            plan_type: account.plan_type().map(str::to_owned),
        }
    }

    fn with_quota(mut self, quota: QuotaState) -> Self {
        self.quota = quota;
        self
    }
}

fn rebuild_account(previous: &ProviderAccount, replacement: AccountReplacement) -> ProviderAccount {
    ProviderAccount::new(
        previous.id().clone(),
        previous.provider().clone(),
        replacement.name,
        previous.upstream_user_id().map(str::to_owned),
        previous.authentication_kind().to_owned(),
        replacement.revision,
        replacement.access_token_expires_at,
    )
    .with_profile(
        replacement.email,
        previous.upstream_account_id().map(str::to_owned),
        replacement.plan_type,
    )
    .with_account_facts(
        replacement.enabled,
        replacement.credential_state,
        replacement.quota,
        replacement.last_error_reason,
        replacement.last_error_message,
    )
    .with_refresh_schedule(replacement.has_refresh_token, replacement.next_refresh_at)
}

#[derive(Default)]
pub struct MemoryGrokCatalogCache {
    entries: Mutex<BTreeMap<GrokCatalogScope, GrokPlanCatalog>>,
}

impl MemoryGrokCatalogCache {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl GrokCredentialCatalogCache for MemoryGrokCatalogCache {
    async fn replace(&self, catalog: GrokPlanCatalog) -> Result<(), GrokCatalogCacheError> {
        lock(&self.entries).insert(catalog.scope().clone(), catalog);
        Ok(())
    }

    async fn read(
        &self,
        scope: &GrokCatalogScope,
    ) -> Result<Option<GrokPlanCatalog>, GrokCatalogCacheError> {
        Ok(lock(&self.entries).get(scope).cloned())
    }

    async fn observed_model_support(
        &self,
        scope: &GrokCatalogScope,
        model: &str,
    ) -> Result<Option<bool>, GrokCatalogCacheError> {
        Ok(lock(&self.entries)
            .get(scope)
            .map(|catalog| catalog.seed().permits(model)))
    }
}

pub fn account_id(suffix: &str) -> ProviderAccountId {
    ProviderAccountId::new(format!("acct_{suffix}")).expect("valid test account ID")
}

pub fn profile(subject: &str) -> GrokAccountProfile {
    let now = Utc::now();
    GrokAccountProfile {
        subject: subject.to_owned(),
        email: Some(format!("{subject}@example.com")),
        upstream_account_id: None,
        plan_type: Some("standard".to_owned()),
        access_token_expires_at: now + chrono::Duration::hours(1),
        refresh_token_expires_at: Some(now + chrono::Duration::days(30)),
    }
}

pub fn create_input(suffix: &str, subject: &str) -> CreateGrokCredential {
    CreateGrokCredential {
        account_id: account_id(suffix),
        name: format!("xAI {suffix}"),
        secret: GrokOAuthSecret {
            access_token: SecretValue::new(format!("access-{suffix}")),
            refresh_token: SecretValue::new(format!("refresh-{suffix}")),
            id_token: Some(SecretValue::new(format!("id-{suffix}"))),
            scope: provider_xai::OFFICIAL_SCOPES.join(" "),
        },
        account: profile(subject),
        enabled: true,
        initial_credential_state: CredentialState::Unknown,
        initial_error_reason: None,
        initial_error_message: None,
    }
}

pub struct StaticRuntimePolicy;

pub struct TestSessionAffinity;

pub struct TestSessionExclusions;

#[derive(Default)]
pub struct MemoryCooldownPort {
    cooldowns: Mutex<BTreeMap<ProviderAccountId, ProviderCooldown>>,
    scoped_cooldowns:
        Mutex<BTreeMap<(ProviderAccountId, ProviderCooldownScope), ProviderScopedCooldown>>,
}

impl MemoryCooldownPort {
    pub fn cooldown(&self, account_id: &ProviderAccountId) -> Option<ProviderCooldown> {
        lock(&self.cooldowns).get(account_id).cloned()
    }

    pub fn scoped_cooldown(
        &self,
        account_id: &ProviderAccountId,
        scope: &ProviderCooldownScope,
    ) -> Option<ProviderScopedCooldown> {
        lock(&self.scoped_cooldowns)
            .get(&(account_id.clone(), scope.clone()))
            .cloned()
    }
}

impl ProviderSessionAffinityPort for TestSessionAffinity {
    fn load<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderAccountId>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn bind<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _account_id: &'a ProviderAccountId,
        _ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn clear<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
}

impl ProviderSessionExclusionPort for TestSessionExclusions {
    fn load<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderSessionExclusions>, ProviderStoreError>>
    {
        Box::pin(async { Ok(None) })
    }

    fn record_failure<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _account_id: &'a ProviderAccountId,
        _ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<ProviderSessionExclusions, ProviderStoreError>> {
        Box::pin(async {
            Ok(ProviderSessionExclusions::new(
                BTreeSet::new(),
                String::new(),
            ))
        })
    }

    fn clear<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _expected_revision: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
}

impl ProviderCooldownPort for MemoryCooldownPort {
    fn put_if_later(
        &self,
        cooldown: ProviderCooldown,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let mut cooldowns = lock(&self.cooldowns);
            let should_write = cooldowns.get(cooldown.account_id()).is_none_or(|current| {
                current.credential_revision() < cooldown.credential_revision()
                    || (current.credential_revision() == cooldown.credential_revision()
                        && current.until() < cooldown.until())
            });
            if should_write {
                cooldowns.insert(cooldown.account_id().clone(), cooldown);
            }
            Ok(should_write)
        })
    }

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async move { Ok(self.cooldown(account_id)) })
    }

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let mut cooldowns = lock(&self.cooldowns);
            let should_remove = cooldowns
                .get(account_id)
                .is_some_and(|current| current.credential_revision() <= through_revision);
            if should_remove {
                cooldowns.remove(account_id);
            }
            Ok(should_remove)
        })
    }

    fn put_scoped_if_later(
        &self,
        cooldown: ProviderScopedCooldown,
    ) -> futures::future::BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let key = (cooldown.account_id().clone(), cooldown.scope().clone());
            let mut cooldowns = lock(&self.scoped_cooldowns);
            let should_write = cooldowns.get(&key).is_none_or(|current| {
                current.credential_revision() < cooldown.credential_revision()
                    || (current.credential_revision() == cooldown.credential_revision()
                        && current.until() < cooldown.until())
            });
            if should_write {
                cooldowns.insert(key, cooldown);
            }
            Ok(should_write)
        })
    }

    fn read_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>>
    {
        Box::pin(async move { Ok(self.scoped_cooldown(account_id, scope)) })
    }

    fn clear_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
        through_revision: CredentialRevision,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let key = (account_id.clone(), scope.clone());
            let mut cooldowns = lock(&self.scoped_cooldowns);
            let should_remove = cooldowns
                .get(&key)
                .is_some_and(|current| current.credential_revision() <= through_revision);
            if should_remove {
                cooldowns.remove(&key);
            }
            Ok(should_remove)
        })
    }

    fn clear_all<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let mut cooldowns = lock(&self.cooldowns);
            let mut scoped = lock(&self.scoped_cooldowns);
            let account_removed = cooldowns.remove(account_id).is_some();
            let scoped_removed = scoped.keys().any(|(id, _)| id == account_id);
            scoped.retain(|(id, _), _| id != account_id);
            Ok(account_removed || scoped_removed)
        })
    }
}

impl ProviderRuntimePolicyPort for StaticRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async { Ok(refresh_policy()) })
    }
}

pub fn refresh_policy() -> ProviderRefreshPolicy {
    ProviderRefreshPolicy::try_new(
        Duration::from_secs(5 * 60),
        NonZeroU32::new(2).expect("positive concurrency"),
    )
    .expect("valid refresh policy")
}

pub fn runtime_policy() -> Arc<dyn ProviderRuntimePolicyPort> {
    Arc::new(StaticRuntimePolicy)
}

pub fn prepare_input(
    input: &CreateGrokCredential,
) -> Result<NewProviderAccount, GrokCredentialRepositoryError> {
    GrokCredentialAdmin.prepare_import(input)
}

pub async fn seed_input(
    store: &MemoryProviderAccountStore,
    input: &CreateGrokCredential,
) -> Result<(), StoreError> {
    let prepared = prepare_input(input).expect("valid xAI account fixture");
    store.create_account(prepared).await
}

pub fn credential_object(
    credential: &PlaintextCredential,
) -> &serde_json::Map<String, serde_json::Value> {
    credential.expose_to_provider()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn conflict() -> StoreError {
    StoreError::new(StoreErrorKind::Conflict)
}

fn invalid() -> StoreError {
    StoreError::new(StoreErrorKind::InvalidData)
}
