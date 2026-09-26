use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use futures::future::BoxFuture;

use gateway_core::operation::OperationKind;
use gateway_core::policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::snapshot::{
    RuntimeSnapshotCompileError, RuntimeSnapshotCompiler, SnapshotAccountGroupFacts,
    SnapshotAccountGroupMemberFacts, SnapshotClientPolicyFacts, SnapshotFacts,
    SnapshotProviderAccountFacts, SnapshotSettingsFacts, SnapshotStoreError, SnapshotStorePort,
};
use gateway_core::routing::{
    ConfigRevision, ContributedModelAlias, ModelCapabilities, ModelPresentation,
    ProviderCatalogGeneration, ProviderCatalogPort, ProviderCatalogUnavailable, ProviderKind,
    ProviderModelCapabilities, PublicModelId, UpstreamModelId,
};
use gateway_core::runtime::extensions::{
    ExtensionPreparationError, ExtensionPreparationPort, ExtensionSetId, ExtensionSetLease,
    ExtensionSetReference,
};

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

struct SequencedSnapshotStore {
    facts: Vec<SnapshotFacts>,
    loads: AtomicUsize,
    current_revision: ConfigRevision,
}

impl SnapshotStorePort for SequencedSnapshotStore {
    fn load_snapshot_facts(&self) -> BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>> {
        Box::pin(async move {
            let index = self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(self.facts[index.min(self.facts.len() - 1)].clone())
        })
    }

    fn current_config_revision(&self) -> BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>> {
        Box::pin(async move { Ok(self.current_revision) })
    }
}

struct PublishingCatalog {
    generation: AtomicU64,
    queries: AtomicUsize,
}

struct TestExtensionLease {
    drops: Arc<AtomicUsize>,
}

impl Drop for TestExtensionLease {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl ExtensionSetLease for TestExtensionLease {
    fn is_ready(&self) -> bool {
        true
    }
}

struct CountingExtensionPreparation {
    prepares: AtomicUsize,
    drops: Arc<AtomicUsize>,
}

struct AliasLease(Vec<ContributedModelAlias>);

impl ExtensionSetLease for AliasLease {
    fn is_ready(&self) -> bool {
        true
    }
    fn model_aliases(&self) -> &[ContributedModelAlias] {
        &self.0
    }
}

struct AliasPreparation(ExtensionSetReference);

impl ExtensionPreparationPort for AliasPreparation {
    fn prepare(
        &self,
        _: ConfigRevision,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

fn alias(id: &str, target: &str) -> ContributedModelAlias {
    ContributedModelAlias {
        owner: "test-plugin".into(),
        id: PublicModelId::new(id).unwrap(),
        provider: ProviderKind::new("alpha").unwrap(),
        target: UpstreamModelId::new(target).unwrap(),
    }
}

fn compile_aliases(
    aliases: Vec<ContributedModelAlias>,
) -> Result<gateway_core::routing::RuntimeSnapshot, RuntimeSnapshotCompileError> {
    let facts = SnapshotFacts::new(
        revision(1),
        revision(1),
        SnapshotSettingsFacts::new(
            3,
            50,
            "smart",
            BTreeMap::from([("configured".into(), "upstream-model".into())]),
            None,
            None,
        ),
        vec![],
        vec![],
        vec![SnapshotProviderAccountFacts::new(
            gateway_core::account::ProviderAccountId::new("acct_alias").unwrap(),
            "alpha",
        )],
        vec![],
    );
    block_on(
        RuntimeSnapshotCompiler::new(
            Arc::new(TestSnapshotStore::new(Ok(facts))),
            Arc::new(PublishingCatalog {
                generation: AtomicU64::new(1),
                queries: AtomicUsize::new(1),
            }),
        )
        .with_extensions(Arc::new(AliasPreparation(ExtensionSetReference::new(
            ExtensionSetId::new("alias-generation".into()).unwrap(),
            Arc::new(AliasLease(aliases)),
        ))))
        .compile(),
    )
}

#[test]
fn contributed_alias_uses_the_same_target_for_listing_profiles_and_routing() {
    let snapshot = compile_aliases(vec![alias("plugin-model", "upstream-model")]).unwrap();
    let model = PublicModelId::new("plugin-model").unwrap();
    let provider = ProviderKind::new("alpha").unwrap();
    assert!(
        snapshot
            .public_models_for_scope(&snapshot.all_account_scope())
            .contains(&model)
    );
    assert!(snapshot.contains_public_model_for_scope(&model, &snapshot.all_account_scope()));
    let profiles = snapshot.public_model_profiles_for_provider(&provider);
    let alias_profile = profiles
        .iter()
        .find(|profile| profile.model() == &model)
        .unwrap();
    let native_profile = profiles
        .iter()
        .find(|profile| profile.model().as_str() == "upstream-model")
        .unwrap();
    assert_eq!(alias_profile.presentation(), native_profile.presentation());
    let plan = snapshot
        .plan(
            &model,
            &super::operation(),
            snapshot.all_account_scope(),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(plan.candidates()[0].provider(), &provider);
    assert_eq!(
        plan.candidates()[0].upstream_model().unwrap().as_str(),
        "upstream-model"
    );
}

#[test]
fn contributed_alias_rejects_collisions_chains_and_missing_targets_before_publication() {
    for aliases in [
        vec![alias("upstream-model", "upstream-model")],
        vec![alias("configured", "upstream-model")],
        vec![alias("plugin-model", "configured")],
        vec![alias("plugin-model", "missing")],
        vec![
            alias("plugin-model", "other"),
            alias("other", "plugin-model"),
        ],
        vec![
            alias("plugin-model", "upstream-model"),
            alias("plugin-model", "upstream-model"),
        ],
    ] {
        assert_eq!(
            compile_aliases(aliases).unwrap_err(),
            RuntimeSnapshotCompileError::InvalidExtensionModels
        );
    }
}

impl ExtensionPreparationPort for CountingExtensionPreparation {
    fn prepare(
        &self,
        _: ConfigRevision,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>> {
        let sequence = self.prepares.fetch_add(1, Ordering::SeqCst) + 1;
        let lease = Arc::new(TestExtensionLease {
            drops: Arc::clone(&self.drops),
        });
        Box::pin(async move {
            Ok(ExtensionSetReference::new(
                ExtensionSetId::new(format!("prepared-{sequence}")).unwrap(),
                lease,
            ))
        })
    }
}

enum TestCatalog {
    NoProviders,
    Unavailable,
    Empty,
    Discovery,
}

impl ProviderCatalogPort for TestCatalog {
    fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        match self {
            Self::NoProviders => BTreeMap::new(),
            Self::Unavailable | Self::Empty | Self::Discovery => catalog_generations(0),
        }
    }

    fn query_model_capabilities(
        &self,
        _: &ProviderKind,
    ) -> BoxFuture<'_, Result<Vec<ProviderModelCapabilities>, ProviderCatalogUnavailable>> {
        Box::pin(async move {
            match self {
                Self::NoProviders | Self::Unavailable => Err(ProviderCatalogUnavailable),
                Self::Empty => Ok(Vec::new()),
                Self::Discovery => Ok(vec![ProviderModelCapabilities::new(
                    UpstreamModelId::new("listed-model").expect("model"),
                    ModelCapabilities::new(
                        std::collections::BTreeSet::from([OperationKind::Generate]),
                        None,
                    ),
                )]),
            }
        })
    }

    fn model_catalog_is_exhaustive(&self, _: &ProviderKind) -> bool {
        !matches!(self, Self::Discovery)
    }
}

fn catalog_generations(generation: u64) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
    BTreeMap::from([(
        ProviderKind::new("alpha").expect("provider"),
        ProviderCatalogGeneration::new(generation),
    )])
}

impl ProviderCatalogPort for PublishingCatalog {
    fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        catalog_generations(self.generation.load(Ordering::SeqCst))
    }

    fn query_model_capabilities(
        &self,
        _: &ProviderKind,
    ) -> BoxFuture<'_, Result<Vec<ProviderModelCapabilities>, ProviderCatalogUnavailable>> {
        Box::pin(async move {
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
        })
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
fn compiler_accepts_unlimited_default_account_concurrency() {
    let facts = SnapshotFacts::new(
        revision(1),
        revision(1),
        SnapshotSettingsFacts::new(0, 50, "smart", BTreeMap::new(), None, None),
        Vec::new(),
        Vec::new(),
        vec![SnapshotProviderAccountFacts::new(
            gateway_core::account::ProviderAccountId::new("acct_unlimited").expect("account"),
            "alpha",
        )],
        Vec::new(),
    );
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(TestSnapshotStore::new(Ok(facts))),
        Arc::new(TestCatalog::Unavailable),
    );
    let snapshot = block_on(compiler.compile()).expect("compile unlimited default");
    let plan = snapshot
        .plan(
            &PublicModelId::new("any-model").expect("model"),
            &super::operation(),
            snapshot.all_account_scope(),
            &Default::default(),
        )
        .expect("plan request");
    assert_eq!(
        plan.account_selection_policy().max_concurrent_per_account(),
        gateway_core::account::AccountConcurrency::Unlimited
    );
    assert_eq!(
        plan.account_selection_policy().request_interval(),
        std::time::Duration::from_millis(50)
    );
}

#[test]
fn compiled_plans_keep_their_smart_config_after_a_new_snapshot_is_built() {
    use gateway_core::account::SmartSchedulingConfig;
    let configs = [
        SmartSchedulingConfig::default(),
        SmartSchedulingConfig::new([0.0, 2.0, 1.0, 0.5, 1.2, 2.3], true).unwrap(),
    ];
    let mut plans = Vec::new();
    for (index, config) in configs.into_iter().enumerate() {
        let facts = SnapshotFacts::new(
            revision(index as u64 + 1),
            revision(index as u64 + 1),
            SnapshotSettingsFacts::new(3, 0, "smart", BTreeMap::new(), None, None)
                .with_smart_scheduling(config),
            vec![],
            vec![],
            vec![SnapshotProviderAccountFacts::new(
                gateway_core::account::ProviderAccountId::new("acct_config").unwrap(),
                "alpha",
            )],
            vec![],
        );
        let compiler = RuntimeSnapshotCompiler::new(
            Arc::new(TestSnapshotStore::new(Ok(facts))),
            Arc::new(TestCatalog::Unavailable),
        );
        let snapshot = block_on(compiler.compile()).unwrap();
        plans.push(
            snapshot
                .plan(
                    &PublicModelId::new("any-model").unwrap(),
                    &super::operation(),
                    snapshot.all_account_scope(),
                    &Default::default(),
                )
                .unwrap(),
        );
    }
    for (plan, config) in plans.iter().zip(configs) {
        assert_eq!(plan.account_selection_policy().smart_scheduling(), config);
    }
}

#[test]
fn withdrawn_provider_keeps_accounts_without_becoming_a_route_or_expanding_group_scope() {
    use gateway_core::{
        account::ProviderAccountId,
        error::RoutingError,
        routing::{AccountGroupId, RoutingContext},
    };

    let retired = ProviderKind::new("retired").unwrap();
    let retired_account = ProviderAccountId::new("acct_retired").unwrap();
    let active_account = ProviderAccountId::new("acct_active").unwrap();
    let group = AccountGroupId::new("grp_00000000000000000000000000000001").unwrap();
    let model = PublicModelId::new("listed-model").unwrap();
    for restricted in [false, true] {
        let facts = SnapshotFacts::new(
            revision(1),
            revision(1),
            SnapshotSettingsFacts::new(
                3,
                0,
                "smart",
                BTreeMap::from([("alias".into(), "listed-model".into())]),
                None,
                None,
            ),
            vec![SnapshotClientPolicyFacts::new(
                ClientApiKeyId::new("key_retired").unwrap(),
                PlaintextClientApiKey::new("sk_retired_test").unwrap(),
                if restricted {
                    vec![group.clone()]
                } else {
                    vec![]
                },
                RateLimits::unlimited(),
            )],
            vec![SnapshotAccountGroupFacts::new(
                group.clone(),
                "Retired".into(),
                true,
            )],
            vec![
                SnapshotProviderAccountFacts::new(retired_account.clone(), "retired"),
                SnapshotProviderAccountFacts::new(active_account.clone(), "alpha"),
            ],
            vec![SnapshotAccountGroupMemberFacts::new(
                group.clone(),
                retired_account.clone(),
            )],
        );
        let snapshot = block_on(
            RuntimeSnapshotCompiler::new(
                Arc::new(TestSnapshotStore::new(Ok(facts))),
                Arc::new(TestCatalog::Discovery),
            )
            .compile(),
        )
        .unwrap();
        let scope = snapshot
            .client_policies()
            .next()
            .unwrap()
            .account_scope()
            .clone();
        assert!(scope.allows(&retired_account));
        assert_eq!(scope.allows(&active_account), !restricted);
        assert!(snapshot.public_models_for_provider(&retired).is_empty());
        let plan = snapshot.plan(
            &model,
            &super::operation(),
            scope.clone(),
            &Default::default(),
        );
        if restricted {
            assert!(matches!(plan, Err(RoutingError::NoCapableProvider { .. })));
            assert!(snapshot.public_models_for_scope(&scope).is_empty());
        } else {
            let plan = plan.unwrap();
            assert_eq!(plan.candidates().len(), 1);
            assert_eq!(plan.candidates()[0].provider().as_str(), "alpha");
        }
        let forced = RoutingContext {
            required_provider: Some(retired.clone()),
            ..Default::default()
        };
        assert!(matches!(
            snapshot.plan(&model, &super::operation(), scope.clone(), &forced),
            Err(RoutingError::NoCapableProvider { .. })
        ));
        assert!(matches!(
            snapshot.plan_provider_endpoint(&retired, None, &super::operation(), scope, &forced),
            Err(RoutingError::NoCapableProviderEndpoint { .. })
        ));
    }
}

#[test]
fn routing_plans_share_frozen_pricing_after_a_new_snapshot_is_published() {
    use gateway_core::{metering::PricingOverrides, runtime::RuntimeSnapshotHandle};
    let prices = |bps| -> Arc<PricingOverrides> {
        Arc::new(
            serde_json::from_value(serde_json::json!({"openai":{"gpt-5.5":{
                "multiplierBps":bps,"bands":{}
            }}}))
            .unwrap(),
        )
    };
    let original = prices(12500);
    let handle = RuntimeSnapshotHandle::new(super::snapshot().with_pricing(original.clone()));
    let frozen = handle.acquire().unwrap();
    let plan = |snapshot: &gateway_core::routing::RuntimeSnapshot| {
        snapshot
            .plan(
                &PublicModelId::new("gpt-5.4").unwrap(),
                &super::operation(),
                snapshot.all_account_scope(),
                &gateway_core::routing::RoutingContext::default(),
            )
            .unwrap()
    };
    let old_plan = plan(&frozen);
    handle.publish(super::snapshot().with_pricing(prices(20000)));
    let new_plan = plan(&handle.acquire().unwrap());
    assert!(Arc::ptr_eq(&old_plan.pricing(), &original));
    assert!(Arc::ptr_eq(&plan(&frozen).pricing(), &original));
    assert_eq!(
        old_plan.pricing()["openai"]["gpt-5.5"].multiplier_bps,
        12500
    );
    assert_eq!(
        new_plan.pricing()["openai"]["gpt-5.5"].multiplier_bps,
        20000
    );
}

#[test]
fn compiler_should_preserve_passthrough_when_provider_catalog_is_unavailable() {
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))),
        Arc::new(TestCatalog::Unavailable),
    );

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
fn discovery_catalog_should_not_reject_an_unlisted_model() {
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))),
        Arc::new(TestCatalog::Discovery),
    );
    let snapshot = block_on(compiler.compile()).expect("compile discovery catalog");
    let provider = ProviderKind::new("alpha").expect("provider");
    let snapshot = snapshot.with_account_directory(Arc::new(
        gateway_core::routing::RuntimeAccountDirectory::new(BTreeMap::from([(
            gateway_core::account::ProviderAccountId::new("acct_discovery").expect("account"),
            gateway_core::routing::RuntimeAccount::new(provider.clone(), Default::default()),
        )])),
    ));
    let model = PublicModelId::new("unknown-upstream-model").expect("model");
    assert!(snapshot.contains_public_model_for_provider(&model, &provider));
    snapshot
        .plan(
            &model,
            &super::operation(),
            snapshot.all_account_scope(),
            &gateway_core::routing::RoutingContext {
                required_provider: Some(provider),
                ..gateway_core::routing::RoutingContext::default()
            },
        )
        .expect("upstream decides model availability");
}

#[test]
fn known_empty_catalog_should_report_model_not_found() {
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))),
        Arc::new(TestCatalog::Empty),
    );
    let snapshot = block_on(compiler.compile()).expect("compile empty catalog");
    let error = snapshot
        .plan(
            &PublicModelId::new("public-model").expect("model"),
            &super::operation(),
            snapshot.all_account_scope(),
            &gateway_core::routing::RoutingContext {
                required_provider: Some(ProviderKind::new("alpha").expect("provider")),
                ..gateway_core::routing::RoutingContext::default()
            },
        )
        .expect_err("a successfully published empty catalog proves model absence");

    assert_eq!(
        error,
        gateway_core::error::RoutingError::ModelNotFound {
            model: "public-model".to_owned(),
            mapped_model: "upstream-model".to_owned(),
        },
    );
}

#[test]
fn compiler_retries_when_provider_publishes_catalog_during_compilation() {
    let catalog = Arc::new(PublishingCatalog {
        generation: AtomicU64::new(0),
        queries: AtomicUsize::new(0),
    });
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))),
        catalog.clone(),
    );

    let snapshot = block_on(compiler.compile()).expect("stable catalog snapshot");

    assert_eq!(catalog.queries.load(Ordering::SeqCst), 2);
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
fn catalog_stability_retry_reuses_and_retains_extensions_for_the_same_revision() {
    let catalog = Arc::new(PublishingCatalog {
        generation: AtomicU64::new(0),
        queries: AtomicUsize::new(0),
    });
    let drops = Arc::new(AtomicUsize::new(0));
    let extensions = Arc::new(CountingExtensionPreparation {
        prepares: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(TestSnapshotStore::new(Ok(facts(3, 3)))),
        catalog.clone(),
    )
    .with_extensions(extensions.clone());

    let snapshot = block_on(compiler.compile()).expect("stable catalog snapshot");

    assert_eq!(catalog.queries.load(Ordering::SeqCst), 2);
    assert_eq!(
        extensions.prepares.load(Ordering::SeqCst),
        1,
        "catalog generation retry must not prepare a second extension set"
    );
    assert_eq!(snapshot.extensions().unwrap().id().as_str(), "prepared-1");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(snapshot);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn catalog_stability_retry_reprepares_extensions_after_revision_change() {
    let catalog = Arc::new(PublishingCatalog {
        generation: AtomicU64::new(0),
        queries: AtomicUsize::new(0),
    });
    let drops = Arc::new(AtomicUsize::new(0));
    let extensions = Arc::new(CountingExtensionPreparation {
        prepares: AtomicUsize::new(0),
        drops: Arc::clone(&drops),
    });
    let compiler = RuntimeSnapshotCompiler::new(
        Arc::new(SequencedSnapshotStore {
            facts: vec![facts(1, 1), facts(2, 2)],
            loads: AtomicUsize::new(0),
            current_revision: revision(2),
        }),
        catalog,
    )
    .with_extensions(extensions.clone());

    let snapshot = block_on(compiler.compile()).expect("new revision snapshot");

    assert_eq!(snapshot.revision(), revision(2));
    assert_eq!(snapshot.extensions().unwrap().id().as_str(), "prepared-2");
    assert_eq!(extensions.prepares.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(snapshot);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
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
    RuntimeSnapshotCompiler::new(store, Arc::new(TestCatalog::NoProviders))
}

fn revision(value: u64) -> ConfigRevision {
    ConfigRevision::new(value).expect("positive revision")
}

#[test]
fn global_request_location_should_be_frozen_when_snapshot_is_published() {
    use gateway_core::account::{ProviderAccountId, RequestLocation};
    use gateway_core::runtime::RuntimeSnapshotHandle;
    let make_snapshot = |version, timezone: &str, enabled| {
        let location = RequestLocation {
            timezone: timezone.parse().unwrap(),
            ..RequestLocation::default()
        };
        let facts = SnapshotFacts::new(
            revision(version),
            revision(version),
            SnapshotSettingsFacts::new(3, 0, "smart", BTreeMap::new(), None, None)
                .with_request_location(location, enabled),
            Vec::new(),
            Vec::new(),
            vec![SnapshotProviderAccountFacts::new(
                ProviderAccountId::new("acct_location").unwrap(),
                "alpha",
            )],
            Vec::new(),
        );
        block_on(
            RuntimeSnapshotCompiler::new(
                Arc::new(TestSnapshotStore::new(Ok(facts))),
                Arc::new(TestCatalog::Unavailable),
            )
            .compile(),
        )
        .unwrap()
    };
    let handle = RuntimeSnapshotHandle::new(make_snapshot(1, "Asia/Tokyo", true));
    let frozen = handle.acquire().unwrap();
    let plan = |snapshot: &gateway_core::routing::RuntimeSnapshot| {
        snapshot
            .plan(
                &PublicModelId::new("public-model").unwrap(),
                &super::operation(),
                snapshot.all_account_scope(),
                &gateway_core::routing::RoutingContext::default(),
            )
            .unwrap()
    };
    let old_plan = plan(&frozen);
    handle.publish(make_snapshot(2, "America/New_York", true));
    assert_eq!(
        old_plan.request_location().unwrap().timezone.name(),
        "Asia/Tokyo"
    );
    assert_eq!(
        plan(&frozen).request_location().unwrap().timezone.name(),
        "Asia/Tokyo"
    );
    assert_eq!(
        plan(&handle.acquire().unwrap())
            .request_location()
            .unwrap()
            .timezone
            .name(),
        "America/New_York"
    );
    handle.publish(make_snapshot(3, "Asia/Tokyo", false));
    assert!(
        plan(&handle.acquire().unwrap())
            .request_location()
            .is_none()
    );
    assert_eq!(
        old_plan.request_location().unwrap().timezone.name(),
        "Asia/Tokyo"
    );
    handle.publish(make_snapshot(4, "Asia/Tokyo", true));
    assert_eq!(
        plan(&handle.acquire().unwrap())
            .request_location()
            .unwrap()
            .timezone
            .name(),
        "Asia/Tokyo"
    );
}

#[test]
fn decompression_setting_should_validate_and_remain_frozen_across_publication() {
    use gateway_core::runtime::RuntimeSnapshotHandle;
    let compile = |version, bytes| {
        let facts = SnapshotFacts::new(
            revision(version),
            revision(version),
            SnapshotSettingsFacts::new(3, 0, "smart", BTreeMap::new(), None, None)
                .with_responses_max_decompressed_body_bytes(bytes),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        block_on(compiler(Arc::new(TestSnapshotStore::new(Ok(facts)))).compile())
    };
    for invalid in [0, u64::MAX] {
        assert!(matches!(
            compile(1, invalid),
            Err(RuntimeSnapshotCompileError::InvalidData)
        ));
    }
    let handle = RuntimeSnapshotHandle::new(compile(1, 64 * 1024 * 1024).unwrap());
    let frozen = handle.acquire().unwrap();
    handle.publish(compile(2, 128 * 1024 * 1024).unwrap());
    assert_eq!(
        handle
            .acquire()
            .unwrap()
            .responses_max_decompressed_body_bytes(),
        128 * 1024 * 1024
    );
    assert_eq!(
        frozen.responses_max_decompressed_body_bytes(),
        64 * 1024 * 1024
    );
    handle.publish(compile(3, 1024 * 1024).unwrap());
    assert_eq!(
        handle
            .acquire()
            .unwrap()
            .responses_max_decompressed_body_bytes(),
        1024 * 1024
    );
}

#[test]
fn disable_fast_uses_only_bound_groups_without_changing_account_scope() {
    use gateway_core::account::ProviderAccountId;
    use gateway_core::routing::AccountGroupId;
    for disable_fast in [false, true] {
        for group_enabled in [false, true] {
            for bound in [false, true] {
                let group_id = AccountGroupId::new("grp_00000000000000000000000000000001").unwrap();
                let open_group_id =
                    AccountGroupId::new("grp_00000000000000000000000000000002").unwrap();
                let account_id = ProviderAccountId::new("acct_fast_policy").unwrap();
                let facts = SnapshotFacts::new(
                    revision(1),
                    revision(1),
                    SnapshotSettingsFacts::new(3, 0, "smart", BTreeMap::new(), None, None),
                    vec![SnapshotClientPolicyFacts::new(
                        ClientApiKeyId::new("key_fast_policy").unwrap(),
                        PlaintextClientApiKey::new("sk_fast_policy").unwrap(),
                        if bound {
                            vec![group_id.clone(), open_group_id.clone()]
                        } else {
                            Vec::new()
                        },
                        RateLimits::unlimited(),
                    )],
                    vec![
                        SnapshotAccountGroupFacts::new(
                            group_id.clone(),
                            "Restricted".to_owned(),
                            group_enabled,
                        )
                        .with_disable_fast(disable_fast),
                        SnapshotAccountGroupFacts::new(
                            open_group_id.clone(),
                            "Open".to_owned(),
                            true,
                        ),
                    ],
                    vec![SnapshotProviderAccountFacts::new(
                        account_id.clone(),
                        "alpha",
                    )],
                    vec![
                        SnapshotAccountGroupMemberFacts::new(group_id, account_id.clone()),
                        SnapshotAccountGroupMemberFacts::new(open_group_id, account_id.clone()),
                    ],
                );
                let snapshot = block_on(
                    RuntimeSnapshotCompiler::new(
                        Arc::new(TestSnapshotStore::new(Ok(facts))),
                        Arc::new(TestCatalog::Unavailable),
                    )
                    .compile(),
                )
                .unwrap();
                let scope = snapshot
                    .client_policies()
                    .next()
                    .unwrap()
                    .account_scope()
                    .clone();
                assert!(scope.allows(&account_id));
                let plan = snapshot
                    .plan(
                        &PublicModelId::new("public-model").unwrap(),
                        &super::operation(),
                        scope,
                        &Default::default(),
                    )
                    .unwrap();
                assert_eq!(
                    plan.disable_fast(),
                    disable_fast && bound,
                    "disable_fast={disable_fast}, enabled={group_enabled}, bound={bound}"
                );
            }
        }
    }
}

#[test]
fn key_profiles_replace_whole_global_choice_and_previous_snapshot_stays_frozen() {
    use gateway_core::account::OpaqueProviderData;
    let provider = ProviderKind::new("alpha").unwrap();
    let document = |label| {
        OpaqueProviderData::new(
            serde_json::json!({"choice":label})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    let build = |global, overridden| {
        let profiles = BTreeMap::from([(provider.clone(), document(global))]);
        let settings = SnapshotSettingsFacts::new(3, 0, "smart", BTreeMap::new(), None, None)
            .with_request_profiles(profiles);
        let inherited = SnapshotClientPolicyFacts::new(
            ClientApiKeyId::new("key_inherited").unwrap(),
            PlaintextClientApiKey::new("sk_inherited").unwrap(),
            vec![],
            RateLimits::unlimited(),
        );
        let independent = SnapshotClientPolicyFacts::new(
            ClientApiKeyId::new("key_independent").unwrap(),
            PlaintextClientApiKey::new("sk_independent").unwrap(),
            vec![],
            RateLimits::unlimited(),
        )
        .with_request_profiles(if overridden {
            BTreeMap::from([(provider.clone(), document("override"))])
        } else {
            BTreeMap::new()
        });
        block_on(
            compiler(Arc::new(TestSnapshotStore::new(Ok(SnapshotFacts::new(
                revision(1),
                revision(1),
                settings,
                vec![inherited, independent],
                vec![],
                vec![],
                vec![],
            )))))
            .compile(),
        )
        .unwrap()
    };
    let values = |snapshot: &gateway_core::routing::RuntimeSnapshot| {
        let mut values: Vec<_> = snapshot
            .client_policies()
            .map(|policy| {
                policy
                    .account_scope()
                    .request_profile(&provider)
                    .unwrap()
                    .expose_to_provider()["choice"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        values.sort();
        values
    };
    let previous = build("global-a", true);
    assert_eq!(values(&build("global-b", true)), ["global-b", "override"]);
    assert_eq!(values(&previous), ["global-a", "override"]);
    assert_eq!(values(&build("global-b", false)), ["global-b", "global-b"]);
}
