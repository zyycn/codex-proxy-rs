use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::executor::block_on;
use futures::future::BoxFuture;

use gateway_core::engine::credential::{AccountSelectionPolicy, RotationStrategy};
use gateway_core::engine::provider::{
    Provider, ProviderCatalogGeneration, ProviderModelCapabilities, ProviderRegistry,
    ProviderRequest, ProviderStream,
};
use gateway_core::engine::{AttemptContext, UpstreamSendState};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::operation::OperationKind;
use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::snapshot::{
    RuntimeSnapshotCompileError, RuntimeSnapshotCompiler, RuntimeSnapshotHandle,
    RuntimeSnapshotPublisher, SnapshotClientPolicyFacts, SnapshotControl, SnapshotFacts,
    SnapshotRevisionStream, SnapshotSettingsFacts, SnapshotStoreError, SnapshotStorePort,
    SnapshotSubscriptionError, SnapshotSubscriptionPort, runtime_revision_needs_refresh,
};
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, PublicModelId, RuntimeSnapshot,
    UpstreamModelId,
};
use gateway_core::task::WorkerKind;

#[derive(Clone)]
struct TestSnapshotStore {
    facts: Arc<Mutex<Result<SnapshotFacts, SnapshotStoreError>>>,
    current_revision: Arc<Mutex<Result<ConfigRevision, SnapshotStoreError>>>,
}

impl TestSnapshotStore {
    fn new(facts: Result<SnapshotFacts, SnapshotStoreError>) -> Self {
        let current_revision = facts.as_ref().map(facts_revision).map_err(Clone::clone);
        Self {
            facts: Arc::new(Mutex::new(facts)),
            current_revision: Arc::new(Mutex::new(current_revision)),
        }
    }
}

impl SnapshotStorePort for TestSnapshotStore {
    fn load_snapshot_facts(&self) -> BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>> {
        Box::pin(async move { self.facts.lock().expect("facts lock").clone() })
    }

    fn current_config_revision(&self) -> BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>> {
        Box::pin(async move { self.current_revision.lock().expect("revision lock").clone() })
    }
}

#[derive(Default)]
struct TestSnapshotSubscriptions {
    published: Mutex<Vec<ConfigRevision>>,
}

impl SnapshotSubscriptionPort for TestSnapshotSubscriptions {
    fn publish_snapshot_revision(
        &self,
        revision: ConfigRevision,
    ) -> BoxFuture<'_, Result<(), SnapshotSubscriptionError>> {
        Box::pin(async move {
            self.published
                .lock()
                .expect("published lock")
                .push(revision);
            Ok(())
        })
    }

    fn subscribe_snapshot_revisions(
        &self,
    ) -> BoxFuture<'_, Result<SnapshotRevisionStream, SnapshotSubscriptionError>> {
        Box::pin(async move { Ok(Box::pin(futures::stream::empty()) as SnapshotRevisionStream) })
    }
}

struct PublishingCatalogProvider {
    generation: AtomicU64,
    queries: AtomicUsize,
}

struct UnavailableCatalogProvider;

#[async_trait]
impl Provider for UnavailableCatalogProvider {
    fn name(&self) -> &'static str {
        "alpha"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::new(0)
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        ))
    }

    async fn execute(
        &self,
        _: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        ))
    }
}

#[async_trait]
impl Provider for PublishingCatalogProvider {
    fn name(&self) -> &'static str {
        "alpha"
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::new(self.generation.load(Ordering::SeqCst))
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        if self.queries.fetch_add(1, Ordering::SeqCst) == 0 {
            self.generation.store(1, Ordering::SeqCst);
        }
        Ok(vec![ProviderModelCapabilities::new(
            UpstreamModelId::new("upstream-model").expect("model"),
            ModelCapabilities::new(
                std::collections::BTreeSet::from([OperationKind::Generate]),
                128_000,
                None,
            ),
        )])
    }

    async fn execute(
        &self,
        _: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        ))
    }
}

#[test]
fn runtime_revision_reconciliation_should_refresh_missing_or_stale_snapshot() {
    assert!(!runtime_revision_needs_refresh(Some(7), 7));
    assert!(runtime_revision_needs_refresh(Some(6), 7));
    assert!(runtime_revision_needs_refresh(Some(8), 7));
    assert!(runtime_revision_needs_refresh(None, 7));
}

#[test]
fn compiler_should_reject_revision_changed_during_consistent_read() {
    let facts = facts(1, 2);
    let compiler = compiler(Arc::new(TestSnapshotStore::new(Ok(facts))));

    let error = block_on(compiler.compile()).expect_err("revision drift must fail closed");

    assert_eq!(error, RuntimeSnapshotCompileError::RevisionChanged);
}

#[test]
fn compiler_should_preserve_passthrough_when_provider_catalog_is_unavailable() {
    let providers =
        ProviderRegistry::new([Arc::new(UnavailableCatalogProvider) as Arc<dyn Provider>])
            .expect("provider registry");
    let compiler =
        RuntimeSnapshotCompiler::new(Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))), providers);

    let snapshot = block_on(compiler.compile()).expect("compile snapshot");
    let provider = ProviderKind::new("alpha").expect("provider");

    assert_eq!(snapshot.revision().get(), 3);
    assert!(snapshot.contains_public_model_for_provider(
        &PublicModelId::new("unknown-upstream-model").expect("model"),
        &provider,
    ));
    assert_eq!(
        snapshot.mapped_model(&provider, "public-model"),
        "upstream-model"
    );
    assert_eq!(snapshot.client_policies().count(), 1);
}

#[test]
fn compiler_retries_when_provider_publishes_catalog_during_compilation() {
    let provider = Arc::new(PublishingCatalogProvider {
        generation: AtomicU64::new(0),
        queries: AtomicUsize::new(0),
    });
    let providers =
        ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).expect("provider registry");
    let compiler =
        RuntimeSnapshotCompiler::new(Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))), providers);

    let snapshot = block_on(compiler.compile()).expect("stable catalog snapshot");

    assert_eq!(provider.queries.load(Ordering::SeqCst), 2);
    assert_eq!(
        snapshot
            .provider_catalog_generations()
            .get(&ProviderKind::new("alpha").expect("provider"))
            .map(|generation| generation.get()),
        Some(1),
    );
}

#[test]
fn handle_should_keep_request_snapshot_frozen_across_publish() {
    let handle = RuntimeSnapshotHandle::new(empty_snapshot(1));
    let frozen = handle.acquire().expect("initial snapshot");

    handle.publish(empty_snapshot(2));

    assert_eq!(frozen.revision().get(), 1);
    assert_eq!(handle.revision().map(ConfigRevision::get), Some(2));
}

#[test]
fn publisher_should_refresh_locally_and_notify_committed_revision() {
    let store = Arc::new(TestSnapshotStore::new(Ok(facts(2, 2))));
    let compiler = Arc::new(compiler(store));
    let handle = RuntimeSnapshotHandle::new(empty_snapshot(1));
    let subscriptions = Arc::new(TestSnapshotSubscriptions::default());
    let publisher = RuntimeSnapshotPublisher::new(compiler, handle.clone(), subscriptions.clone());

    block_on(publisher.publish_committed(revision(2)));

    assert_eq!(handle.revision().map(ConfigRevision::get), Some(2));
    assert_eq!(
        subscriptions
            .published
            .lock()
            .expect("published lock")
            .as_slice(),
        &[revision(2)],
    );
}

#[test]
fn publisher_should_suspend_but_still_notify_after_committed_refresh_failure() {
    let store = Arc::new(TestSnapshotStore::new(Err(
        SnapshotStoreError::unavailable(),
    )));
    let handle = RuntimeSnapshotHandle::new(empty_snapshot(1));
    let subscriptions = Arc::new(TestSnapshotSubscriptions::default());
    let publisher = RuntimeSnapshotPublisher::new(
        Arc::new(compiler(store)),
        handle.clone(),
        subscriptions.clone(),
    );

    block_on(publisher.publish_committed(revision(2)));

    assert!(handle.acquire().is_err());
    assert_eq!(
        subscriptions
            .published
            .lock()
            .expect("published lock")
            .as_slice(),
        &[revision(2)],
    );
}

#[test]
fn snapshot_ports_and_control_should_remain_object_safe() {
    fn accept_store(_: &dyn SnapshotStorePort) {}
    fn accept_subscriptions(_: &dyn SnapshotSubscriptionPort) {}
    fn accept_control(_: &dyn SnapshotControl) {}

    let store = Arc::new(TestSnapshotStore::new(Ok(facts(1, 1))));
    let subscriptions = Arc::new(TestSnapshotSubscriptions::default());
    let publisher = RuntimeSnapshotPublisher::new(
        Arc::new(compiler(store.clone())),
        RuntimeSnapshotHandle::new(empty_snapshot(1)),
        subscriptions.clone(),
    );

    accept_store(store.as_ref());
    accept_subscriptions(subscriptions.as_ref());
    accept_control(&publisher);
}

#[test]
fn publisher_should_contribute_reconciliation_and_subscription_workers() {
    let store = Arc::new(TestSnapshotStore::new(Ok(facts(1, 1))));
    let publisher = RuntimeSnapshotPublisher::new(
        Arc::new(compiler(store)),
        RuntimeSnapshotHandle::new(empty_snapshot(1)),
        Arc::new(TestSnapshotSubscriptions::default()),
    );

    let contributions = publisher
        .worker_contributions()
        .expect("valid frozen worker definitions");
    let kinds = contributions
        .iter()
        .map(gateway_core::task::WorkerContribution::kind)
        .collect::<Vec<_>>();

    assert_eq!(
        kinds,
        vec![
            WorkerKind::RuntimeSnapshotReconciliation,
            WorkerKind::RuntimeChangeSubscription,
        ],
    );
}

fn facts(config_revision: u64, observed_current_revision: u64) -> SnapshotFacts {
    SnapshotFacts::new(
        revision(config_revision),
        revision(observed_current_revision),
        SnapshotSettingsFacts::new(
            3,
            50,
            "smart",
            BTreeMap::from([(
                "alpha".to_owned(),
                BTreeMap::from([("public-model".to_owned(), "upstream-model".to_owned())]),
            )]),
        ),
        vec![SnapshotClientPolicyFacts::new(
            ClientApiKeyId::new("key_one").expect("key ID"),
            PlaintextClientApiKey::new("sk_test").expect("plaintext key"),
            "alpha",
            RateLimits::unlimited(),
        )],
    )
}

fn facts_revision(facts: &SnapshotFacts) -> ConfigRevision {
    facts.config_revision()
}

fn compiler(store: Arc<dyn SnapshotStorePort>) -> RuntimeSnapshotCompiler {
    RuntimeSnapshotCompiler::new(store, ProviderRegistry::default())
}

fn empty_snapshot(value: u64) -> RuntimeSnapshot {
    RuntimeSnapshot::new(
        revision(value),
        AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(1).expect("positive concurrency"),
            Duration::ZERO,
        ),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty snapshot")
}

fn revision(value: u64) -> ConfigRevision {
    ConfigRevision::new(value).expect("positive revision")
}
