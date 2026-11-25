//! 验证 OpenAI 管理能力、请求画像、Bundle 组装与额度投影

mod peer_info;
mod provider;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeZone as _, Utc};
use futures::{StreamExt, future::BoxFuture};
use gateway_admin::model::accounts::AccountRecord;
use gateway_admin::model::observability::{
    CurrencyCost, DesktopReleaseStatus, ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, CompleteAuthorization,
    ConsumeProviderResetCredit, PendingAuthorizationMutation, PrepareCredentialImport,
    PrepareCredentialRefresh, PrepareCredentialRotation, ProviderDocument,
    ProviderExportCredentialInput, ProviderQuotaReadRequest, ProviderQuotaRequest,
    ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::ProviderAdminErrorKind;
use gateway_core::account::{
    CredentialRevision, CredentialState, OpaqueProviderData, ProviderAccount, ProviderAccountId,
    ProviderAccountStore, QuotaAccessChange, QuotaEvidence, QuotaObservation, QuotaState,
};
use gateway_core::engine::provider::ProviderRequest;
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, ModelRequestId, RequestAttemptContext,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingClaimOutcome, OAuthPendingConsumeOutcome,
    OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingReleaseOutcome,
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderCatalogCacheKey,
    ProviderCatalogCachePort, ProviderCooldown, ProviderCooldownPort, ProviderCooldownScope,
    ProviderCredentialState, ProviderCredentialStatePort, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderScopedCooldown, ProviderStoreError, ProviderStorePorts,
};
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities, ProviderKind,
    ProviderModel, PublicModelId, RoutingContext, RuntimeAccount, RuntimeAccountDirectory,
    RuntimeSnapshot, UpstreamModelId,
};
use gateway_core::task::{WorkerContribution, WorkerKind, WorkerRunnable};
use provider_openai::config::OpenAiConfig;
use provider_openai::credential::{CodexCredentialCodec, ImportCodexOAuthCredential};
use provider_openai::profile::APPCAST_POLL_INTERVAL;
use secrecy::SecretString;
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, MemorySessionAffinity, MemorySessionExclusions, TestLeaseCoordinator,
    account_policy, profile, secret,
};

const COMPLETED_SESSION_SSE: &str = concat!(
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_initialized_session\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
);

async fn reset_credit_admin(
    server: &MockServer,
) -> (
    gateway_admin::ports::provider::ProviderBundle,
    ProviderAccountId,
    TestOpenAiConfig,
) {
    let account_id = ProviderAccountId::new("acct_reset_credit").expect("account ID");
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_string(),
            name: "reset credit".to_owned(),
            secret: secret("reset-credit-access"),
            verified_account: profile("chatgpt-reset-credit"),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let mut config = valid_config();
    config.config.api.base_url = server.uri();
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
        Arc::new(gateway_core::account::UpstreamInfoReader::default()),
    )
    .await
    .expect("OpenAI reset-credit bundle");
    (bundle, account_id, config)
}

fn reset_credit_command(account_id: ProviderAccountId) -> ConsumeProviderResetCredit {
    ConsumeProviderResetCredit {
        account_id,
        credit_id: Some("credit_1".to_owned()),
        redeem_request_id: Uuid::parse_str("8fbf302d-11df-4bd5-82e4-08e4b3df7874")
            .expect("UUID v4"),
    }
}

pub(crate) fn initialized_provider_request(
    operation: Operation,
    account_id: &str,
) -> ProviderRequest {
    let provider = ProviderKind::new("openai").expect("provider");
    let upstream_model = UpstreamModelId::new("gpt-5.4").expect("upstream model");
    let public_model = PublicModelId::new(upstream_model.as_str()).expect("public model");
    let account_scope = initialized_account_scope(account_id);
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        gateway_core::settings::SettingsValues::new(2, "smart", Default::default(), None, None),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            upstream_model,
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
                .with_upstream_feature_validation(),
        )],
        Vec::new(),
    )
    .expect("runtime snapshot");
    let plan = snapshot
        .plan(
            &public_model,
            &operation,
            account_scope,
            &RoutingContext::default(),
        )
        .expect("routing plan");

    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

pub(crate) fn initialized_attempt_context(request_id: &str, account_id: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_initialized").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(initialized_account_scope(account_id)),
        None,
        CancellationToken::new(),
    )
}

fn initialized_account_scope(account_id: &str) -> Arc<FrozenAccountScope> {
    let provider = ProviderKind::new("openai").expect("provider");
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new(account_id).expect("account id"),
            RuntimeAccount::new(provider, BTreeSet::new()),
        )]))),
        ClientRoutingScope::all_accounts(),
    ))
}

pub(crate) async fn initialized_test_provider(
    accounts: Arc<MemoryAccountStore>,
    base_url: String,
) -> Arc<dyn gateway_core::engine::provider::Provider> {
    initialized_test_bundle(accounts, base_url)
        .await
        .core
        .clone()
}

pub(crate) async fn initialized_test_bundle(
    accounts: Arc<MemoryAccountStore>,
    base_url: String,
) -> gateway_admin::ports::provider::ProviderBundle {
    let mut config = valid_config();
    config.config.api.base_url = base_url;
    provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(accounts, Arc::new(TestOAuthPending::default())),
        Arc::new(gateway_core::account::UpstreamInfoReader::default()),
    )
    .await
    .expect("initialized provider")
}

fn provider_ports() -> ProviderStorePorts {
    provider_ports_with(
        Arc::new(MemoryAccountStore::default()),
        Arc::new(TestOAuthPending::default()),
    )
}

fn provider_ports_with(
    accounts: Arc<MemoryAccountStore>,
    pending: Arc<TestOAuthPending>,
) -> ProviderStorePorts {
    provider_ports_with_catalog(accounts, pending, Arc::new(TestCatalogCache::default()))
}

fn provider_ports_with_catalog(
    accounts: Arc<MemoryAccountStore>,
    pending: Arc<TestOAuthPending>,
    catalog_cache: Arc<TestCatalogCache>,
) -> ProviderStorePorts {
    ProviderStorePorts::new(
        accounts,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(MemorySessionExclusions::default()),
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
        last_error_message: None,
        created_at: now,
        updated_at: now,
    }
}

struct TestOpenAiConfig {
    config: OpenAiConfig,
    _runtime: TempDir,
}

fn valid_config() -> TestOpenAiConfig {
    let mut config = OpenAiConfig::default();
    let runtime = tempfile::tempdir().expect("test runtime directory");
    config
        .resolve_and_validate(&runtime.path().join("deploy"))
        .expect("valid OpenAI test configuration");
    TestOpenAiConfig {
        config,
        _runtime: runtime,
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
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
        claim: &'a gateway_core::provider_ports::OAuthPendingBinding,
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
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
        claim: &'a gateway_core::provider_ports::OAuthPendingBinding,
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
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
        claim: &'a gateway_core::provider_ports::OAuthPendingBinding,
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
    use gateway_core::provider_ports::{
        ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderSchedulingState,
    };

    use super::*;

    #[tokio::test]
    async fn expired_token_prompts_refresh_before_quota_and_profile_queries() {
        let store = Arc::new(MemoryAccountStore::default());
        let account_id = ProviderAccountId::new("acct_expired").unwrap();
        let mut verified_account = profile("chatgpt-expired");
        verified_account.access_token_expires_at = Some(Utc::now() - chrono::Duration::minutes(1));
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: account_id.as_str().to_owned(),
                name: "expired".to_owned(),
                secret: secret("expired-test-token"),
                verified_account,
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        let server = MockServer::start().await;
        let provider = initialized_test_bundle(store, server.uri())
            .await
            .admin
            .clone();
        let quota_error = provider
            .quota(ProviderQuotaRequest {
                account_id: account_id.clone(),
                refresh: true,
                rolling_usage: None,
            })
            .await
            .unwrap_err();
        let profile_error = provider.profile_statistics(&account_id).await.unwrap_err();
        for error in [quota_error, profile_error] {
            assert_eq!(error.public_message(), Some("请先刷新令牌"));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn quota_refresh_exposes_safe_rejections_and_recovers_after_revoked_token_rotation() {
        let store = Arc::new(MemoryAccountStore::default());
        let account_id = "acct_quota_error";
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: account_id.to_owned(),
                name: "quota error".to_owned(),
                secret: secret("quota-error-test-token"),
                verified_account: profile("chatgpt-quota-error"),
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        let mut before = store.account(account_id).expect("account");
        let server = MockServer::start().await;
        let mut config = valid_config();
        config.config.api.base_url = server.uri();
        let bundle = provider_openai::initialize(
            config.config,
            provider_ports_with(store.clone(), Arc::new(TestOAuthPending::default())),
            Arc::new(gateway_core::account::UpstreamInfoReader::default()),
        )
        .await
        .expect("OpenAI bundle");
        for (status, code, expected_kind, expected_message) in [
            (
                401,
                "token_revoked",
                Kind::BadGateway,
                "访问令牌已撤销，请刷新令牌或重新授权",
            ),
            (
                401,
                "unknown_code",
                Kind::BadGateway,
                "账号认证失败，请检查授权状态",
            ),
            (
                403,
                "forbidden",
                Kind::BadGateway,
                "额度查询被拒绝，请检查账号权限",
            ),
            (
                429,
                "rate_limited",
                Kind::BadGateway,
                "额度查询过于频繁，请稍后重试",
            ),
            (
                503,
                "unavailable",
                Kind::BadGateway,
                "额度服务异常，请稍后重试",
            ),
            (400, "invalid_request", Kind::Unavailable, "额度查询失败"),
        ] {
            server.reset().await;
            Mock::given(method("GET"))
                .and(path("/api/codex/usage"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "error": {"code": code, "message": "raw-secret-marker"}
                })))
                .mount(&server)
                .await;
            let error = bundle
                .admin
                .quota(ProviderQuotaRequest {
                    account_id: before.id().clone(),
                    refresh: true,
                    rolling_usage: None,
                })
                .await
                .expect_err("quota rejection");
            assert_eq!(error.kind(), expected_kind);
            assert_eq!(error.public_message(), Some(expected_message));
            assert!(!format!("{error:?} {error}").contains("raw-secret-marker"));
            let current = store.account(account_id).expect("account after rejection");
            if code == "token_revoked" {
                assert_eq!(
                    current
                        .status_projection(SystemTime::now(), None)
                        .error_reason,
                    Some(gateway_core::account::AccountErrorReason::AccessTokenExpired)
                );
                store
                    .repository()
                    .rotate_refreshed_oauth_secret(
                        &before,
                        secret("replacement-test-token"),
                        before.access_token_expires_at(),
                        None,
                    )
                    .await
                    .unwrap();
                before = store.account(account_id).expect("account after rotation");
            } else {
                assert_eq!(current, before);
            }
        }
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rate_limit": {"allowed": true, "primary_window": {"used_percent": 8}}
            })))
            .mount(&server)
            .await;
        let quota = bundle
            .admin
            .quota(ProviderQuotaRequest {
                account_id: before.id().clone(),
                refresh: true,
                rolling_usage: None,
            })
            .await
            .expect("quota recovered");
        assert_eq!(quota.representative_used_percent(), Some(8.0));
        let current = store.account(account_id).expect("account after recovery");
        assert_eq!(current.credential_state(), CredentialState::Ready);
        assert!(current.last_error_message().is_none());
    }

    #[tokio::test]
    async fn manual_refresh_preserves_banned_evidence_without_promoting_401_to_terminal() {
        for (status, kind, message) in [
            (400, Kind::Invalid, "上游账号已停用"),
            (401, Kind::BadGateway, "令牌刷新被拒绝，请检查账号授权"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "error": {
                        "message": "account has been deactivated: raw-secret-marker"
                    }
                })))
                .expect(1)
                .mount(&server)
                .await;
            let (bundle, store, _config) = refresh_fixture(&server, false, true).await;
            let before = store.account("acct_refresh_error").unwrap();
            let error = bundle
                .admin
                .prepare_refresh(PrepareCredentialRefresh {
                    account: account_record(&before),
                })
                .await
                .unwrap_err();
            assert_eq!(error.kind(), kind);
            assert_eq!(error.public_message(), Some(message));
            assert!(!format!("{error:?} {error}").contains("raw-secret-marker"));
            assert_eq!(store.account("acct_refresh_error").unwrap(), before);
        }
    }

    #[tokio::test]
    async fn manual_refresh_reports_known_upstream_failures_without_changing_account_state() {
        for (status, code, expected_kind, expected_message) in [
            (
                401,
                "refresh_token_reused",
                Kind::BadGateway,
                "刷新令牌已被使用，请重新授权",
            ),
            (
                401,
                "refresh_token_expired",
                Kind::BadGateway,
                "刷新令牌已过期，请重新授权",
            ),
            (
                401,
                "refresh_token_invalidated",
                Kind::BadGateway,
                "刷新令牌已被撤销，请重新授权",
            ),
            (
                401,
                "token_expired",
                Kind::BadGateway,
                "刷新令牌不可用，请重新授权",
            ),
            (
                401,
                "unknown",
                Kind::BadGateway,
                "令牌刷新被拒绝，请检查账号授权",
            ),
            (
                400,
                "refresh_token_reused",
                Kind::Invalid,
                "刷新令牌已被使用，请重新授权",
            ),
            (
                400,
                "refresh_token_expired",
                Kind::Invalid,
                "刷新令牌已过期，请重新授权",
            ),
            (
                400,
                "refresh_token_invalidated",
                Kind::Invalid,
                "刷新令牌已被撤销，请重新授权",
            ),
            (
                400,
                "INVALID_GRANT",
                Kind::BadGateway,
                "刷新令牌无效，请重新授权",
            ),
            (
                429,
                "unknown",
                Kind::BadGateway,
                "令牌刷新过于频繁，请稍后重试",
            ),
            (
                503,
                "unknown",
                Kind::BadGateway,
                "令牌刷新服务异常，请稍后重试",
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "error": {"code": code, "message": "raw-secret-marker"}
                })))
                .expect(1)
                .mount(&server)
                .await;
            let (bundle, store, _config) = refresh_fixture(&server, false, true).await;
            let before = store.account("acct_refresh_error").unwrap();
            let error = bundle
                .admin
                .prepare_refresh(PrepareCredentialRefresh {
                    account: account_record(&before),
                })
                .await
                .expect_err("upstream rejection");
            assert_eq!(error.kind(), expected_kind, "HTTP {status} {code}");
            assert_eq!(error.public_message(), Some(expected_message));
            assert!(!format!("{error:?} {error}").contains("raw-secret-marker"));
            assert_eq!(store.account("acct_refresh_error").unwrap(), before);
        }
    }

    #[tokio::test]
    async fn manual_refresh_distinguishes_busy_missing_token_and_stale_account_before_exchange() {
        for (busy, has_token, stale, kind, message) in [
            (
                true,
                true,
                false,
                Kind::Conflict,
                "令牌刷新繁忙，请稍后重试",
            ),
            (
                false,
                false,
                false,
                Kind::Invalid,
                "账号没有刷新令牌，请重新授权",
            ),
            (
                false,
                true,
                true,
                Kind::Conflict,
                "凭据已更新，请刷新账号列表",
            ),
        ] {
            let server = MockServer::start().await;
            let (bundle, store, _config) = refresh_fixture(&server, busy, has_token).await;
            let before = store.account("acct_refresh_error").unwrap();
            let mut account = account_record(&before);
            if stale {
                account.upstream_user_id = Some("previous-user".to_owned());
            }
            let error = bundle
                .admin
                .prepare_refresh(PrepareCredentialRefresh { account })
                .await
                .expect_err("local refresh failure");
            assert_eq!(error.kind(), kind);
            assert_eq!(error.public_message(), Some(message));
            assert!(server.received_requests().await.unwrap().is_empty());
            assert_eq!(store.account("acct_refresh_error").unwrap(), before);
        }
    }

    #[tokio::test]
    async fn manual_refresh_invalid_success_and_unclassified_transport_stay_conservative() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("invalid-success-secret-marker"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (bundle, store, mut config) = refresh_fixture(&server, false, true).await;
        let before = store.account("acct_refresh_error").unwrap();
        let command = || PrepareCredentialRefresh {
            account: account_record(&before),
        };
        let error = bundle.admin.prepare_refresh(command()).await.unwrap_err();
        assert_eq!(error.kind(), Kind::Ambiguous);
        assert_eq!(
            error.public_message(),
            Some("刷新结果未知，核对账号状态前请勿重试")
        );
        assert!(!format!("{error:?}").contains("invalid-success-secret-marker"));

        config.config.auth.oauth_token_endpoint = "http://127.0.0.1:0/oauth/token".to_owned();
        let bundle = provider_openai::initialize(
            config.config,
            refresh_ports(store.clone(), false),
            Arc::new(gateway_core::account::UpstreamInfoReader::default()),
        )
        .await
        .unwrap();
        let error = bundle.admin.prepare_refresh(command()).await.unwrap_err();
        assert_eq!(error.kind(), Kind::Ambiguous);
        assert_eq!(
            error.public_message(),
            Some("刷新结果未知，核对账号状态前请勿重试")
        );
        assert_eq!(store.account("acct_refresh_error").unwrap(), before);
    }

    #[tokio::test]
    async fn manual_token_refresh_prepares_to_preserve_concurrent_profile_changes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new-access-token", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (bundle, store, _config) = refresh_fixture(&server, false, true).await;
        let before = store.account("acct_refresh_error").unwrap();
        let prepared = bundle
            .admin
            .prepare_refresh(PrepareCredentialRefresh {
                account: account_record(&before),
            })
            .await
            .unwrap();
        assert!(prepared.facts().preserve_profile);
    }

    async fn refresh_fixture(
        server: &MockServer,
        busy: bool,
        has_token: bool,
    ) -> (
        gateway_admin::ports::provider::ProviderBundle,
        Arc<MemoryAccountStore>,
        TestOpenAiConfig,
    ) {
        let store = Arc::new(MemoryAccountStore::default());
        let mut credential = secret("synthetic-refresh-access");
        if !has_token {
            credential.refresh_token = None;
        }
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: "acct_refresh_error".to_owned(),
                name: "refresh error test".to_owned(),
                secret: credential,
                verified_account: profile("synthetic-refresh-user"),
                next_refresh_at: None,
                enabled: true,
            })
            .await;
        let mut config = valid_config();
        config.config.auth.oauth_token_endpoint = format!("{}/oauth/token", server.uri());
        let bundle = provider_openai::initialize(
            config.config.clone(),
            refresh_ports(store.clone(), busy),
            Arc::new(gateway_core::account::UpstreamInfoReader::default()),
        )
        .await
        .unwrap();
        (bundle, store, config)
    }

    fn refresh_ports(store: Arc<MemoryAccountStore>, busy: bool) -> ProviderStorePorts {
        ProviderStorePorts::new(
            store,
            Arc::new(RefreshLeases { busy }),
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(MemorySessionExclusions::default()),
            Arc::new(TestCatalogCache::default()),
            Arc::new(TestArtifactProfiles),
            Arc::new(TestCredentialState),
            Arc::new(TestCooldown),
            Arc::new(TestRuntimePolicy),
            Arc::new(TestOAuthPending::default()),
            Arc::new(RecordingDiagnostics::default()),
        )
    }

    struct RefreshLeases {
        busy: bool,
    }

    impl ProviderLeasePort for RefreshLeases {
        fn load_state<'a>(
            &'a self,
            _: &'a ClientApiKeyId,
            _: &'a ProviderKind,
            _: &'a [ProviderAccountId],
            _pool: gateway_core::provider_ports::ProviderConcurrencyPool,
        ) -> BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
            panic!("manual refresh does not use scheduling leases")
        }

        fn try_acquire(
            &self,
            request: ProviderLeaseRequest,
        ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
            assert!(matches!(
                request,
                ProviderLeaseRequest::Refresh(_) | ProviderLeaseRequest::RefreshCapacity(_)
            ));
            Box::pin(async move {
                Ok(if self.busy {
                    ProviderLeaseAcquisition::Busy { retry_after: None }
                } else {
                    ProviderLeaseAcquisition::Acquired(Box::new(()))
                })
            })
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
