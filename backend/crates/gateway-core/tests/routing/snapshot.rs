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
    ConfigRevision, ModelCapabilities, ModelPresentation, ProviderCatalogGeneration,
    ProviderCatalogPort, ProviderCatalogUnavailable, ProviderKind, ProviderModelCapabilities,
    PublicModelId, UpstreamModelId,
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

struct PublishingCatalog {
    generation: AtomicU64,
    queries: AtomicUsize,
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
fn disable_fast_uses_bound_groups_and_global_policy_without_changing_account_scope() {
    use gateway_core::account::ProviderAccountId;
    use gateway_core::routing::AccountGroupId;
    for global in [false, true] {
        for group_enabled in [false, true] {
            for bound in [false, true] {
                let group_id = AccountGroupId::new("grp_00000000000000000000000000000001").unwrap();
                let open_group_id =
                    AccountGroupId::new("grp_00000000000000000000000000000002").unwrap();
                let account_id = ProviderAccountId::new("acct_fast_policy").unwrap();
                let facts = SnapshotFacts::new(
                    revision(1),
                    revision(1),
                    SnapshotSettingsFacts::new(3, 0, "smart", BTreeMap::new(), None, None)
                        .with_disable_fast(global),
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
                        .with_disable_fast(true),
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
                    global || bound,
                    "global={global}, enabled={group_enabled}, bound={bound}"
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
