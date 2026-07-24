use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use gateway_admin::model::accounts::{
    AccountAvailability as AdminAccountAvailability, AccountRecord,
};
use gateway_admin::model::observability::{CurrencyCost, ProviderBillingInput};
use gateway_admin::model::provider_credentials::{
    CompleteAuthorization, PrepareCredentialImport, PrepareCredentialRefresh,
    PrepareCredentialRotation, ProviderDocument, ProviderExportCredentialInput,
    ProviderQuotaRequest,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::ProviderAdminErrorKind;
use gateway_core::engine::credential::{
    AccountAvailability, AccountRuntimeSignals, AccountSelectionPolicy, CredentialRevision,
    NewProviderAccount, OpaqueProviderData, PlaintextCredential, ProviderAccount,
    ProviderAccountId, ProviderAccountStore, RotationStrategy,
};
use gateway_core::engine::provider::ProviderRequest;
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId, RequestAttemptContext,
};
use gateway_core::event::GatewayEvent;
use gateway_core::operation::{Operation, OperationKind};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingFlowPort, OAuthPendingPutOutcome,
    OAuthPendingTakeOutcome, ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCooldown,
    ProviderCooldownPort, ProviderCredentialState, ProviderCredentialStatePort,
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderStoreError, ProviderStorePorts,
};
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext,
    RuntimeSnapshot, UpstreamModelId,
};
use gateway_core::task::{
    WorkerContribution, WorkerCycleContext, WorkerKind, WorkerRunnable, WorkerTaskError,
};
use provider_xai::{
    DiscoveryDocument, GrokOAuthConfig, PendingAuthorization, RedirectUriAllowlist,
};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::support::{
    MemoryProviderAccountStore, TestSessionAffinity, create_input, seed_input, xai_config,
};

#[tokio::test]
#[ignore = "requires XAI_REAL_ACCOUNT_FIXTURE and consumes live xAI quota; refresh token may rotate"]
async fn real_xai_conversation_crosses_production_provider_boundaries() {
    assert_eq!(
        std::env::var("XAI_ALLOW_DESTRUCTIVE_FIXTURE_REFRESH").as_deref(),
        Ok("1"),
        "set XAI_ALLOW_DESTRUCTIVE_FIXTURE_REFRESH=1 for the disposable real fixture",
    );
    let fixture = std::env::var("XAI_REAL_ACCOUNT_FIXTURE")
        .expect("XAI_REAL_ACCOUNT_FIXTURE must point to a Grok JSON document");
    let payload = serde_json::from_slice::<Value>(&std::fs::read(fixture).expect("read fixture"))
        .expect("parse fixture");
    let Value::Object(document) = payload else {
        panic!("real xAI fixture must be an object");
    };
    let store = Arc::new(MemoryProviderAccountStore::default());
    let bundle = provider_xai::initialize(
        xai_config(),
        provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("xAI bundle");
    let prepared = bundle
        .admin_provider()
        .prepare_import(PrepareCredentialImport {
            document: ProviderDocument::new(OpaqueProviderData::new(document)),
        })
        .await
        .expect("prepare real xAI import");
    for credential in prepared.credentials {
        store
            .create_account(real_prepared_account(credential))
            .await
            .expect("seed verified xAI account");
    }

    let models = bundle
        .core_provider()
        .query_model_capabilities()
        .await
        .expect("query live xAI catalog");
    let model = models.first().expect("one live generation model");
    let operation = bundle
        .admin_provider()
        .connection_test_operation(model.upstream_model(), "Reply with exactly CPR_REAL_OK.")
        .expect("real connection operation");
    let request = real_planned_request(model.upstream_model().clone(), operation);
    let mut stream = bundle
        .core_provider()
        .execute(request, real_xai_attempt_context())
        .await
        .expect("prepare real xAI stream");
    let mut started = false;
    let mut completed = false;
    let mut usage = false;
    let mut text = false;
    while let Some(event) = stream.next().await {
        for fact in event.expect("real xAI event").canonical_facts() {
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

fn real_prepared_account(
    value: gateway_admin::model::provider_credentials::PreparedCredentialCreate,
) -> NewProviderAccount {
    let account = ProviderAccount::new(
        value.account_id,
        value.provider_kind,
        value.name,
        value.upstream_user_id,
        value.authentication_kind,
        CredentialRevision::new(1).expect("initial revision"),
        value.access_token_expires_at.map(Into::into),
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

fn real_planned_request(upstream_model: UpstreamModelId, operation: Operation) -> ProviderRequest {
    let provider = ProviderKind::new("xai").expect("provider");
    let public_model = PublicModelId::new(upstream_model.as_str()).expect("public model");
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        real_selection_policy(),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            upstream_model,
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None),
        )],
        vec![],
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(&public_model, &operation, &RoutingContext::default())
        .expect("route plan");
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn real_selection_policy() -> AccountSelectionPolicy {
    AccountSelectionPolicy::new(
        RotationStrategy::Smart,
        NonZeroU32::new(1).expect("concurrency"),
        Duration::ZERO,
    )
}

fn real_xai_attempt_context() -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_real_xai_conversation").expect("request"),
            ClientApiKeyId::new("real_test_client").expect("client"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(90),
        real_selection_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn xai_bundle_exposes_core_admin_and_drains_worker_contributions_once() {
    let mut bundle = provider_xai::initialize(xai_config(), provider_ports())
        .await
        .expect("xAI bundle");

    assert_eq!(bundle.core_provider().name(), "xai");
    assert_eq!(bundle.admin_provider().provider_kind().as_str(), "xai");
    let contributions = bundle.take_worker_contributions();
    assert_eq!(contributions.len(), 3);
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
    assert!(bundle.take_worker_contributions().is_empty());
}

#[tokio::test]
async fn xai_quota_catalog_worker_treats_empty_account_pool_as_idle() {
    let mut bundle = provider_xai::initialize(xai_config(), provider_ports())
        .await
        .expect("xAI bundle");

    assert_eq!(run_quota_catalog_cycle(&mut bundle).await, Ok(()));
}

#[tokio::test]
async fn xai_quota_catalog_worker_preserves_store_failures() {
    let store = Arc::new(MemoryProviderAccountStore::default());
    store.fail_provider_listing();
    let mut bundle = provider_xai::initialize(
        xai_config(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("xAI bundle");

    let error = run_quota_catalog_cycle(&mut bundle)
        .await
        .expect_err("Provider account Store failure");
    assert_eq!(error.as_safe_str(), "xAI Provider accounts unavailable");
}

async fn run_quota_catalog_cycle(
    bundle: &mut provider_xai::ProviderBundle,
) -> Result<(), WorkerTaskError> {
    let registration = bundle
        .take_worker_contributions()
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

#[tokio::test]
async fn xai_admin_provider_validates_known_billing_breakdown() {
    let bundle = provider_xai::initialize(xai_config(), provider_ports())
        .await
        .expect("xAI bundle");
    let admin = bundle.admin_provider();
    let profile = admin.dashboard_wire_profile().expect("wire profile");
    assert_eq!(profile.provider, "xai");
    assert_eq!(profile.product, "Grok Build");
    assert_eq!(profile.version, "0.2.106");
    assert_eq!(profile.user_agent, "grok-shell/0.2.106 (linux; x86_64)");
    assert!(profile.release.is_none());

    let billing = admin
        .calculated_billing(&ProviderBillingInput {
            upstream_model_id: "grok-4.5".to_owned(),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cached_tokens: Some(25),
            cache_write_tokens: Some(0),
            total: CurrencyCost {
                currency: "USD".to_owned(),
                amount: "0.0002175".parse().expect("amount"),
            },
        })
        .expect("billing")
        .expect("known pricing");

    assert_eq!(billing.total_amount.amount.as_str(), "0.0002175");
    assert_eq!(billing.input_price_per_million.amount.as_str(), "2");
    assert_eq!(billing.output_price_per_million.amount.as_str(), "6");
}

#[tokio::test]
async fn xai_admin_provider_restores_full_pending_envelope_and_binds_owner() {
    let pending = Arc::new(TestOAuthPending::default());
    let bundle = provider_xai::initialize(
        xai_config(),
        provider_ports_with(
            Arc::new(MemoryProviderAccountStore::default()),
            Arc::clone(&pending),
        ),
    )
    .await
    .expect("xAI bundle");
    let owner = MutationContext {
        actor: MutationActor::AdminSession {
            admin_user_id: "admin-owner".to_owned(),
        },
        request_id: "request-start".to_owned(),
    };
    let flow_id = "pending-envelope";
    let owner_ref = admin_session_owner_ref("admin-owner");
    let payload = pending_payload(flow_id, &owner_ref);
    pending.insert(flow_id, owner_ref, payload.clone());

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

    let wrong_owner = bundle
        .admin_provider()
        .complete_authorization(CompleteAuthorization {
            context: MutationContext {
                actor: MutationActor::AdminSession {
                    admin_user_id: "different-owner".to_owned(),
                },
                request_id: "request-complete".to_owned(),
            },
            flow_id: flow_id.to_owned(),
            callback_url: format!(
                "{}?code=unused&state=wrong-owner",
                provider_xai::OFFICIAL_REDIRECT_URI
            ),
        })
        .await
        .expect_err("wrong owner");
    assert_eq!(wrong_owner.kind(), ProviderAdminErrorKind::NotFound);
    assert_eq!(pending.len(), 1);

    let invalid_callback = bundle
        .admin_provider()
        .complete_authorization(CompleteAuthorization {
            context: owner,
            flow_id: flow_id.to_owned(),
            callback_url: format!(
                "{}?code=unused&state=invalid-state",
                provider_xai::OFFICIAL_REDIRECT_URI
            ),
        })
        .await
        .expect_err("restored pending must validate callback state");
    assert_eq!(invalid_callback.kind(), ProviderAdminErrorKind::Invalid);
    assert_eq!(pending.len(), 0);

    let mismatched_flow = "pending-envelope-mismatch";
    let owner_ref = admin_session_owner_ref("admin-owner");
    pending.insert(
        mismatched_flow,
        owner_ref.clone(),
        pending_payload("different-envelope", &owner_ref),
    );
    let mismatched_envelope = bundle
        .admin_provider()
        .complete_authorization(CompleteAuthorization {
            context: MutationContext {
                actor: MutationActor::AdminSession {
                    admin_user_id: "admin-owner".to_owned(),
                },
                request_id: "request-complete-mismatch".to_owned(),
            },
            flow_id: mismatched_flow.to_owned(),
            callback_url: format!(
                "{}?code=unused&state=invalid-state",
                provider_xai::OFFICIAL_REDIRECT_URI
            ),
        })
        .await
        .expect_err("flow binding mismatch");
    assert_eq!(mismatched_envelope.kind(), ProviderAdminErrorKind::Invalid);
    assert_eq!(pending.len(), 0);
}

#[tokio::test]
async fn xai_admin_provider_projects_cached_quota_models_and_canonical_export() {
    let store = Arc::new(MemoryProviderAccountStore::default());
    let input = create_input("admin_projection", "subject-admin-projection");
    seed_input(&store, &input).await.expect("create account");
    let account = store.account(&input.account_id).expect("stored account");
    let record = account_record(&account);
    let bundle = provider_xai::initialize(
        xai_config(),
        provider_ports_with(Arc::clone(&store), Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("xAI bundle");
    let admin = bundle.admin_provider();

    let operation = admin
        .connection_test_operation(
            &UpstreamModelId::new("grok-4.5").expect("upstream model"),
            "Reply with exactly OK.",
        )
        .expect("connection test operation");
    let Operation::Generate(request) = operation else {
        panic!("connection test must be a generate operation");
    };
    let encoded = provider_xai::GrokResponsesRequest::encode(
        &request,
        "grok-4.5",
        &ClientApiKeyId::new("admin_connection_test").expect("client key"),
    )
    .expect("official xAI request");
    assert_eq!(
        encoded.body().get("model").and_then(Value::as_str),
        Some("grok-4.5")
    );
    assert_eq!(
        encoded.body().get("stream").and_then(Value::as_bool),
        Some(true)
    );

    let quota = admin
        .quota(ProviderQuotaRequest {
            account_id: account.id().clone(),
            refresh: false,
            rolling_usage: None,
        })
        .await
        .expect("cached quota");
    assert!(quota.windows.is_empty());
    let models = admin
        .models(account.id(), false)
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
    assert_eq!(exported.account_ids, vec![input.account_id]);
    let document = exported.document.expose_to_provider().expose_to_provider();
    assert_eq!(document.get("version").and_then(Value::as_u64), Some(1));
    assert_eq!(
        document.get("type").and_then(Value::as_str),
        Some("oauth-account-bundle")
    );
}

#[tokio::test]
async fn xai_admin_provider_rejects_unprepared_mutations_before_store_commit() {
    let store = Arc::new(MemoryProviderAccountStore::default());
    let input = create_input("admin_invalid", "subject-admin-invalid");
    seed_input(&store, &input).await.expect("create account");
    let account = store.account(&input.account_id).expect("stored account");
    let record = account_record(&account);
    let bundle = provider_xai::initialize(
        xai_config(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .expect("xAI bundle");
    let admin = bundle.admin_provider();

    let import_error = admin
        .prepare_import(PrepareCredentialImport {
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

fn pending_payload(flow_id: &str, owner_ref: &str) -> OpaqueProviderData {
    let config = GrokOAuthConfig::official().expect("official config");
    let discovery = DiscoveryDocument::parse(
        &config,
        include_bytes!("credential/fixtures/discovery.json"),
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
        "schema_version": 1,
        "flow_id": flow_id,
        "owner_ref": owner_ref,
        "expires_at": (Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
        "server_state": server_state,
        "mutation": {
            "expected_config_revision": 7,
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
    ProviderStorePorts::new(
        accounts,
        Arc::new(TestLeases),
        Arc::new(TestSessionAffinity),
        Arc::new(TestCatalogCache::default()),
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
        provider_kind: account.provider().clone(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        authentication_kind: account.authentication_kind().to_owned(),
        credential_revision: Revision::new(account.revision().get()).expect("revision"),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().map(DateTime::<Utc>::from),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        availability: admin_availability(account.availability()),
        availability_reason: None,
        cooldown_until: account.cooldown_until().map(DateTime::<Utc>::from),
        availability_observed_at: now,
        quota_observed_at: None,
        created_at: now,
        updated_at: now,
    }
}

const fn admin_availability(value: AccountAvailability) -> AdminAccountAvailability {
    match value {
        AccountAvailability::Unknown => AdminAccountAvailability::Unknown,
        AccountAvailability::Ready => AdminAccountAvailability::Ready,
        AccountAvailability::Cooldown => AdminAccountAvailability::Cooldown,
        AccountAvailability::QuotaExhausted => AdminAccountAvailability::QuotaExhausted,
        AccountAvailability::Expired => AdminAccountAvailability::Expired,
        AccountAvailability::Banned => AdminAccountAvailability::Banned,
        AccountAvailability::Invalid => AdminAccountAvailability::Invalid,
    }
}

struct TestLeases;

impl ProviderLeasePort for TestLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::routing::ProviderKind,
        accounts: &'a [ProviderAccountId],
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
                            quota_remaining_rank: None,
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

impl TestOAuthPending {
    fn insert(&self, flow_id: &str, owner: String, payload: OpaqueProviderData) {
        self.values.lock().expect("OAuth pending").insert(
            ("xai".to_owned(), flow_id.to_owned()),
            (
                owner,
                payload,
                SystemTime::now() + Duration::from_secs(1_800),
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
                ),
            );
            Ok(OAuthPendingPutOutcome::Stored)
        })
    }

    fn take_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
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
