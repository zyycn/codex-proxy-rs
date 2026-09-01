use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::executor::block_on;
use futures::future::BoxFuture;

use gateway_core::engine::AttemptContext;
use gateway_core::engine::provider::{
    Provider, ProviderCatalogGeneration, ProviderModelCapabilities, ProviderRegistry,
    ProviderRequest, ProviderStream,
};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::operation::OperationKind;
use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::snapshot::{
    RuntimeSnapshotCompileError, RuntimeSnapshotCompiler, SnapshotAccountGroupFacts,
    SnapshotAccountGroupMemberFacts, SnapshotClientPolicyFacts, SnapshotFacts,
    SnapshotProviderAccountFacts, SnapshotSettingsFacts, SnapshotStoreError, SnapshotStorePort,
};
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ModelPresentation, ProviderKind, PublicModelId,
    UpstreamModelId,
};
use gateway_core::upstream::UpstreamSendState;

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
        Ok(vec![
            ProviderModelCapabilities::new(
                UpstreamModelId::new("upstream-model").expect("model"),
                ModelCapabilities::new(
                    std::collections::BTreeSet::from([OperationKind::Generate]),
                    None,
                ),
            )
            .with_presentation(ModelPresentation::new(
                Some("Upstream Model".to_owned()),
                None,
            )),
        ])
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
    assert_eq!(snapshot.mapped_model("public-model"), "upstream-model");
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
    let profiles =
        snapshot.public_model_profiles_for_provider(&ProviderKind::new("alpha").expect("provider"));
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.model().as_str())
            .collect::<Vec<_>>(),
        vec!["public-model", "upstream-model"],
    );
}

#[test]
fn compiler_should_freeze_valid_client_min_versions() {
    let store = Arc::new(TestSnapshotStore::new(Ok(facts_with_min_versions(
        1,
        1,
        Some("26.825.6671".to_owned()),
        Some("0.40.0".to_owned()),
    ))));

    let snapshot = block_on(compiler(store).compile()).expect("valid min versions");

    assert_eq!(
        snapshot
            .min_codex_client_versions()
            .desktop()
            .map(ToString::to_string)
            .as_deref(),
        Some("26.825.6671")
    );
    assert_eq!(
        snapshot
            .min_codex_client_versions()
            .cli()
            .map(ToString::to_string)
            .as_deref(),
        Some("0.40.0")
    );
}

#[test]
fn compiler_should_reject_invalid_persisted_client_min_version() {
    let store = Arc::new(TestSnapshotStore::new(Ok(facts_with_min_versions(
        1,
        1,
        None,
        Some("v0.40.0".to_owned()),
    ))));

    assert_eq!(
        block_on(compiler(store).compile()).expect_err("invalid min version"),
        RuntimeSnapshotCompileError::InvalidData
    );
}

fn facts(config_revision: u64, observed_current_revision: u64) -> SnapshotFacts {
    facts_with_min_versions(config_revision, observed_current_revision, None, None)
}

fn facts_with_min_versions(
    config_revision: u64,
    observed_current_revision: u64,
    desktop: Option<String>,
    cli: Option<String>,
) -> SnapshotFacts {
    SnapshotFacts::new(
        revision(config_revision),
        revision(observed_current_revision),
        SnapshotSettingsFacts::new(
            3,
            50,
            "smart",
            BTreeMap::from([("public-model".to_owned(), "upstream-model".to_owned())]),
            desktop,
            cli,
        ),
        vec![SnapshotClientPolicyFacts::new(
            ClientApiKeyId::new("key_one").expect("key ID"),
            PlaintextClientApiKey::new("sk_test").expect("plaintext key"),
            Vec::new(),
            RateLimits::unlimited(),
        )],
        Vec::<SnapshotAccountGroupFacts>::new(),
        Vec::<SnapshotProviderAccountFacts>::new(),
        Vec::<SnapshotAccountGroupMemberFacts>::new(),
    )
}

fn facts_revision(facts: &SnapshotFacts) -> ConfigRevision {
    facts.config_revision()
}

fn compiler(store: Arc<dyn SnapshotStorePort>) -> RuntimeSnapshotCompiler {
    RuntimeSnapshotCompiler::new(store, ProviderRegistry::default())
}

fn revision(value: u64) -> ConfigRevision {
    ConfigRevision::new(value).expect("positive revision")
}
