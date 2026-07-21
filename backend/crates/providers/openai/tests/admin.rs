use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, TimeZone as _, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use gateway_admin::model::accounts::{
    AccountAvailability as AdminAccountAvailability, AccountRecord,
};
use gateway_admin::model::observability::{
    CurrencyCost, DesktopReleaseStatus, ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, CompleteAuthorization,
    PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
    PrepareCredentialRotation, ProviderDocument, ProviderExportCredentialInput,
    ProviderQuotaRequest,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::ProviderAdminErrorKind;
use gateway_core::engine::credential::{
    AccountAvailability, AccountSelectionPolicy, CredentialRevision, NewProviderAccount,
    OpaqueProviderData, PlaintextCredential, ProviderAccount, ProviderAccountId,
    ProviderAccountStore, RotationStrategy,
};
use gateway_core::engine::provider::ProviderRequest;
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId, RequestAttemptContext,
};
use gateway_core::event::GatewayEvent;
use gateway_core::operation::OperationKind;
use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, Operation, ProviderOptions,
    ReasoningEffort, ReasoningRequirement,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingTakeOutcome,
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCatalogPorts, ProviderCooldown,
    ProviderCooldownPort, ProviderCredentialState, ProviderCredentialStatePort,
    ProviderInstanceCatalogPort, ProviderInstanceConfig, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderStoreError, ProviderStorePorts,
};
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderInstanceId,
    ProviderKind, ProviderModel, PublicModelId, RoutingContext, RuntimeSnapshot, UpstreamModelId,
};
use gateway_core::task::{WorkerContribution, WorkerKind, WorkerRunnable};
use provider_openai::config::{CodexWireProfileConfig, OpenAiConfig};
use provider_openai::credential::CreateCodexCredential;
use provider_openai::transport::profile::APPCAST_POLL_INTERVAL;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

use crate::support::{MemoryAccountStore, TestLeaseCoordinator, instance_id, profile, secret};

#[tokio::test]
#[ignore = "requires CODEX_REAL_ACCOUNT_FIXTURE and consumes live OpenAI quota"]
async fn real_openai_conversation_crosses_production_provider_boundaries() {
    let fixture = std::env::var("CODEX_REAL_ACCOUNT_FIXTURE")
        .expect("CODEX_REAL_ACCOUNT_FIXTURE must point to a CPR JSON document");
    let payload = serde_json::from_slice::<Value>(&std::fs::read(fixture).expect("read fixture"))
        .expect("parse fixture");
    let Value::Object(document) = payload else {
        panic!("real OpenAI fixture must be an object");
    };
    let store = Arc::new(MemoryAccountStore::default());
    let bundle = provider_openai::initialize(
        valid_config(),
        provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");
    let prepared = bundle
        .admin_provider()
        .prepare_import(PrepareCredentialImport {
            provider_instance_id: instance_id(),
            document: ProviderDocument::new(OpaqueProviderData::new(document)),
        })
        .await
        .expect("prepare real OpenAI import");
    for credential in prepared.credentials {
        store
            .create_account(prepared_account(credential))
            .await
            .expect("seed verified OpenAI account");
    }

    let instance = ProviderInstance::new(
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        "https://chatgpt.com/backend-api".to_owned(),
        true,
        InstanceHealth::Healthy,
    );
    let models = bundle
        .core_provider()
        .query_model_capabilities(&instance)
        .await
        .expect("query live OpenAI catalog");
    let model = models.first().expect("one live generation model");
    let operation = bundle
        .admin_provider()
        .connection_test_operation(model.upstream_model(), "Reply with exactly CPR_REAL_OK.")
        .expect("real connection operation");
    let request = planned_real_request(instance, model.upstream_model().clone(), operation);
    let mut stream = bundle
        .core_provider()
        .execute(request, real_attempt_context())
        .await
        .expect("prepare real OpenAI stream");
    let mut started = false;
    let mut completed = false;
    let mut usage = false;
    let mut text = false;
    while let Some(event) = stream.next().await {
        for fact in event.expect("real OpenAI event").canonical_facts() {
            match fact {
                GatewayEvent::Started(_) => started = true,
                GatewayEvent::TextDelta(_) => text = true,
                GatewayEvent::Usage(_) => usage = true,
                GatewayEvent::Completed(_) => completed = true,
                _ => {}
            }
        }
    }
    assert!(started && text && usage && completed);
}

fn prepared_account(
    value: gateway_admin::model::provider_credentials::PreparedCredentialCreate,
) -> NewProviderAccount {
    let account = ProviderAccount::new(
        value.account_id,
        value.provider_instance_id,
        value.provider_kind,
        value.name,
        value.upstream_user_id,
        CredentialRevision::new(1).expect("initial revision"),
        value.access_token_expires_at.into(),
    )
    .with_profile(value.email, value.upstream_account_id, value.plan_type)
    .with_runtime_state(value.enabled, AccountAvailability::Ready, None)
    .with_refresh_schedule(
        value.has_refresh_token,
        value.next_refresh_at.map(Into::into),
    );
    NewProviderAccount {
        account,
        credential: PlaintextCredential::new(
            value.provider_material.into_provider_data().into_inner(),
        ),
    }
}

fn planned_real_request(
    instance: ProviderInstance,
    upstream_model: UpstreamModelId,
    operation: Operation,
) -> ProviderRequest {
    let public_model = PublicModelId::new(upstream_model.as_str()).expect("public model");
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(1).expect("concurrency"),
            Duration::ZERO,
        ),
        vec![instance.clone()],
        vec![ProviderModel::new(
            instance.id().clone(),
            upstream_model,
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), 1_000_000, None),
        )],
        vec![],
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(&public_model, &operation, &RoutingContext::default())
        .expect("route plan");
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn real_attempt_context() -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_real_openai_conversation").expect("request"),
            ClientApiKeyId::new("real_test_client").expect("client"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(90),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(1).expect("concurrency"),
            Duration::ZERO,
        ),
        AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn openai_bundle_exposes_one_core_provider_and_drains_worker_contributions_once() {
    let mut bundle = provider_openai::initialize(valid_config(), provider_ports())
        .await
        .expect("OpenAI bundle");

    assert_eq!(bundle.core_provider().name(), "openai");
    assert_eq!(bundle.admin_provider().provider_kind().as_str(), "openai");
    let contributions = bundle.take_worker_contributions();
    assert_eq!(contributions.len(), 5);
    assert!(
        contributions
            .iter()
            .any(|item| item.kind() == WorkerKind::OAuthRefresh)
    );
    assert!(
        contributions
            .iter()
            .any(|item| item.kind() == WorkerKind::QuotaCatalogHealth)
    );
    let release_worker = contributions
        .iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-desktop-release" =>
            {
                Some(registration)
            }
            WorkerContribution::Registration(_) | WorkerContribution::Disabled { .. } => None,
        })
        .expect("Desktop release worker");
    assert_eq!(release_worker.id.kind(), WorkerKind::QuotaCatalogHealth);
    let WorkerRunnable::Scheduled { schedule, .. } = &release_worker.runnable else {
        panic!("Desktop release worker must be scheduled");
    };
    assert_eq!(schedule.interval(), APPCAST_POLL_INTERVAL);
    assert!(contributions.iter().any(|contribution| {
        matches!(
            contribution,
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "openai-model-etag"
                    && matches!(&registration.runnable, WorkerRunnable::Daemon { .. })
        )
    }));
    assert!(bundle.take_worker_contributions().is_empty());
}

#[tokio::test]
async fn openai_admin_provider_exposes_live_wire_profile_and_validated_billing() {
    let bundle = provider_openai::initialize(valid_config(), provider_ports())
        .await
        .expect("OpenAI bundle");
    let admin = bundle.admin_provider();
    let profile = admin.dashboard_wire_profile().expect("wire profile");
    assert_eq!(
        profile
            .attributes
            .iter()
            .find(|attribute| attribute.label == "Codex Core")
            .map(|attribute| attribute.value.as_str()),
        Some("0.102.0")
    );
    assert_eq!(
        profile.release.as_ref().map(|release| release.status),
        Some(DesktopReleaseStatus::Unchecked)
    );
    let billing = admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "gpt-4o".to_owned(),
            input_tokens: Some(1_000_000),
            output_tokens: Some(0),
            cached_tokens: Some(0),
            cache_write_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "2.5".parse().expect("amount"),
            },
        })
        .expect("billing")
        .expect("known pricing");
    assert_eq!(billing.total_amount.amount.as_str(), "2.5");
    assert_eq!(billing.input_price_per_million.amount.as_str(), "2.5");
}

#[tokio::test]
async fn openai_core_provider_projects_codex_request_observation_without_routing_side_effects() {
    let bundle = provider_openai::initialize(valid_config(), provider_ports())
        .await
        .expect("OpenAI bundle");
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("summarize".to_owned())],
    )
    .expect("message");
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                (
                    "turn_metadata".to_owned(),
                    json!(r#"{"request_kind":"compaction","subagent_kind":"review"}"#),
                ),
            ]),
        )
        .expect("provider options");
    let operation = Operation::Generate(
        GenerateRequest::new(vec![message])
            .expect("generate")
            .with_reasoning(ReasoningRequirement {
                effort: Some(ReasoningEffort::High),
                summary: None,
            })
            .with_provider_options(options),
    );

    let observation = bundle.core_provider().request_observation(&operation);

    assert_eq!(observation.request_kind.as_deref(), Some("compaction"));
    assert_eq!(observation.subagent_kind.as_deref(), Some("review"));
    // Codex 当前只在特定多代理预设组合下给出 reasoning_preset；普通 high 保持空值。
    assert_eq!(observation.reasoning_preset, None);
    assert!(observation.compact);
}

#[tokio::test]
async fn openai_admin_provider_persists_the_full_pending_envelope_and_binds_owner() {
    let pending = Arc::new(TestOAuthPending::default());
    let bundle = provider_openai::initialize(
        valid_config(),
        provider_ports_with(
            Arc::new(MemoryAccountStore::default()),
            Arc::clone(&pending),
        ),
    )
    .await
    .expect("OpenAI bundle");
    let start_context = MutationContext {
        actor: MutationActor::AdminSession {
            admin_user_id: "admin-owner".to_owned(),
        },
        request_id: "request-start".to_owned(),
    };
    let started = bundle
        .admin_provider()
        .start_authorization(PendingAuthorizationMutation::new(
            Revision::new(7).expect("revision"),
            ProviderKind::new("openai").expect("provider"),
            AuthorizationMutationTarget::Create {
                provider_instance_id: instance_id(),
                name: "OAuth account".to_owned(),
            },
            AuthorizationOwnerBinding::from_context(&start_context),
        ))
        .await
        .expect("start authorization");
    {
        let values = pending.values.lock().expect("OAuth pending");
        let (_, payload, _) = values.values().next().expect("stored pending");
        let mutation = payload
            .expose_to_provider()
            .get("mutation")
            .and_then(Value::as_object)
            .expect("pending mutation");
        assert_eq!(
            mutation
                .get("expected_config_revision")
                .and_then(Value::as_u64),
            Some(7)
        );
        assert_eq!(
            mutation.get("started_request_id").and_then(Value::as_str),
            Some("request-start")
        );
    }
    let error = bundle
        .admin_provider()
        .complete_authorization(CompleteAuthorization {
            context: MutationContext {
                actor: MutationActor::AdminSession {
                    admin_user_id: "different-owner".to_owned(),
                },
                request_id: "request-complete".to_owned(),
            },
            flow_id: started.flow_id,
            callback_url: "http://localhost:1455/auth/callback?code=unused&state=unused".to_owned(),
        })
        .await
        .expect_err("wrong owner");
    assert_eq!(error.kind(), ProviderAdminErrorKind::NotFound);
    assert_eq!(pending.values.lock().expect("OAuth pending").len(), 1);
}

#[tokio::test]
async fn openai_admin_provider_projects_cached_quota_models_and_canonical_export() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_admin_projection".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "admin projection".to_owned(),
            secret: secret("admin-projection-access"),
            account: profile("chatgpt-admin-projection"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await
        .expect("create account");
    let account = store
        .account("acct_admin_projection")
        .expect("stored account");
    let record = account_record(&account);
    let bundle = provider_openai::initialize(
        valid_config(),
        provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");
    let admin = bundle.admin_provider();

    let operation = admin
        .connection_test_operation(
            &UpstreamModelId::new("gpt-5.4").expect("upstream model"),
            "Reply with exactly OK.",
        )
        .expect("connection test operation");
    let Operation::Generate(request) = operation else {
        panic!("connection test must be a generate operation");
    };
    let encoded = provider_openai::encode_generate_request(&request, "gpt-5.4")
        .expect("official OpenAI request");
    assert_eq!(
        encoded.body().get("model").and_then(Value::as_str),
        Some("gpt-5.4")
    );
    assert_eq!(
        encoded.body().get("stream").and_then(Value::as_bool),
        Some(true)
    );

    let account_id = account.id().clone();
    let quota = admin
        .quota(ProviderQuotaRequest {
            account_id: account_id.clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("cached quota");
    assert!(quota.windows.is_empty());
    let models = admin
        .models(&account_id, false)
        .await
        .expect("cached models");
    assert!(models.models.is_empty());
    let loaded = store
        .load_credential(account.id(), account.revision())
        .await
        .expect("loaded credential");
    let exported = admin
        .export_credentials(vec![ProviderExportCredentialInput {
            account: record,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(
                loaded.credential.into_inner(),
            )),
        }])
        .await
        .expect("canonical export");
    assert_eq!(exported.account_ids, vec![account_id]);
    assert_eq!(
        exported
            .document
            .expose_to_provider()
            .expose_to_provider()
            .get("sourceFormat")
            .and_then(Value::as_str),
        Some("cpr")
    );
}

#[tokio::test]
async fn openai_admin_provider_rejects_unprepared_mutations_before_store_commit() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .repository()
        .create_oauth_credential(CreateCodexCredential {
            account_id: "acct_admin_invalid".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "admin invalid".to_owned(),
            secret: secret("admin-invalid-access"),
            account: profile("chatgpt-admin-invalid"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await
        .expect("create account");
    let account = store.account("acct_admin_invalid").expect("stored account");
    let record = account_record(&account);
    let bundle = provider_openai::initialize(
        valid_config(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("OpenAI bundle");
    let admin = bundle.admin_provider();
    let import_error = admin
        .prepare_import(PrepareCredentialImport {
            provider_instance_id: instance_id(),
            document: ProviderDocument::new(OpaqueProviderData::new(Map::new())),
        })
        .await
        .expect_err("invalid import");
    assert_eq!(import_error.kind(), ProviderAdminErrorKind::Invalid);
    let rotation_error = admin
        .prepare_rotation(PrepareCredentialRotation {
            account: record.clone(),
            expected_credential_revision: record.credential_revision,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(Map::new())),
        })
        .await
        .expect_err("invalid rotation");
    assert_eq!(rotation_error.kind(), ProviderAdminErrorKind::Invalid);
    let mut missing = record;
    missing.id = "acct_admin_missing".to_owned();
    let refresh_error = admin
        .prepare_refresh(PrepareCredentialRefresh { account: missing })
        .await
        .expect_err("missing refresh target");
    assert_eq!(refresh_error.kind(), ProviderAdminErrorKind::NotFound);
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
    ProviderStorePorts::new(
        accounts,
        Arc::new(TestLeaseCoordinator::default()),
        ProviderCatalogPorts::new(
            Arc::new(TestInstanceCatalog),
            Arc::new(TestCatalogCache::default()),
        ),
        Arc::new(TestCredentialState),
        Arc::new(TestCooldown),
        Arc::new(TestRuntimePolicy),
        pending,
    )
}

fn account_record(account: &ProviderAccount) -> AccountRecord {
    let now = Utc::now();
    AccountRecord {
        id: account.id().to_string(),
        provider_instance_id: account.instance().clone(),
        provider_kind: account.provider().clone(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        credential_revision: Revision::new(account.revision().get()).expect("revision"),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: DateTime::<Utc>::from(account.access_token_expires_at()),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        availability: AdminAccountAvailability::Ready,
        availability_reason: None,
        cooldown_until: account.cooldown_until().map(DateTime::<Utc>::from),
        availability_observed_at: now,
        quota_observed_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn valid_config() -> OpenAiConfig {
    OpenAiConfig {
        wire_profile: CodexWireProfileConfig {
            originator: "Codex Desktop".to_owned(),
            codex_version: "0.102.0".to_owned(),
            desktop_version: "1.2026.190".to_owned(),
            desktop_build: "19012345678".to_owned(),
            os_type: "macOS".to_owned(),
            os_version: "15.5.0".to_owned(),
            arch: "arm64".to_owned(),
            terminal: "xterm-256color".to_owned(),
            verified_at: Utc
                .with_ymd_and_hms(2026, 7, 19, 0, 0, 0)
                .single()
                .expect("valid test time"),
        },
    }
}

struct TestInstanceCatalog;

impl ProviderInstanceCatalogPort for TestInstanceCatalog {
    fn list_instances<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        _include_disabled: bool,
    ) -> BoxFuture<'a, Result<Vec<ProviderInstanceConfig>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(vec![ProviderInstanceConfig::new(
                ProviderInstanceId::new("inst_openai_primary").expect("instance"),
                provider_kind.clone(),
                "https://chatgpt.com/backend-api".to_owned(),
                true,
            )])
        })
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
                .insert(key.account_id().to_string(), catalog.clone());
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
                .get(key.account_id().as_str())
                .cloned())
        })
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
}

struct TestCooldown;

impl ProviderCooldownPort for TestCooldown {
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
type PendingValue = (String, OpaqueProviderData, SystemTime);

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
                ),
            );
            Ok(OAuthPendingPutOutcome::Stored)
        })
    }

    fn take_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a gateway_core::provider_ports::OAuthPendingBinding,
        owner: &'a gateway_core::provider_ports::OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingTakeOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, expires_at)) = values.get(&key) else {
                return Ok(OAuthPendingTakeOutcome::NotFound);
            };
            if expires_at <= &SystemTime::now() {
                values.remove(&key);
                return Ok(OAuthPendingTakeOutcome::NotFound);
            }
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingTakeOutcome::OwnerMismatch);
            }
            let (_, payload, _) = values.remove(&key).expect("checked pending");
            Ok(OAuthPendingTakeOutcome::Taken(payload))
        })
    }
}
