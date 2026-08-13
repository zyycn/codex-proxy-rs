use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use gateway_core::engine::credential::{
    AccountSelectionPolicy, ProviderAccountId, RotationStrategy,
};
use gateway_core::operation::{
    CapabilityRequirements, Feature, GenerateRequest, Operation, OperationKind, ProtocolPayload,
};
use gateway_core::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::{
    AccountGroupId, AccountRoutingScopeKind, ClientRoutingScope, ConfigRevision,
    FrozenAccountScope, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId,
    RoutingContext, RoutingGroupSnapshot, RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot,
    UpstreamModelId,
};

mod snapshot;

fn scheduling() -> AccountSelectionPolicy {
    AccountSelectionPolicy::new(
        RotationStrategy::Smart,
        NonZeroU32::new(3).expect("positive"),
        Duration::from_millis(50),
    )
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000))
}

fn model(provider: &str, name: &str, capabilities: ModelCapabilities) -> ProviderModel {
    ProviderModel::new(
        ProviderKind::new(provider).expect("valid provider"),
        UpstreamModelId::new(name).expect("valid model"),
        capabilities,
    )
}

fn operation() -> Operation {
    let body = serde_json::json!({
        "model": "gpt-5.5",
        "input": [{"type": "message", "role": "user", "content": "hello"}],
    });
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().expect("request object").clone())
            .expect("OpenAI payload"),
    ))
}

fn account_directory() -> Arc<RuntimeAccountDirectory> {
    Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
        (
            ProviderAccountId::new("acct_openai").expect("account"),
            RuntimeAccount::new(
                ProviderKind::new("openai").expect("provider"),
                BTreeSet::new(),
            ),
        ),
        (
            ProviderAccountId::new("acct_xai").expect("account"),
            RuntimeAccount::new(ProviderKind::new("xai").expect("provider"), BTreeSet::new()),
        ),
    ])))
}

fn account_scope() -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        account_directory(),
        ClientRoutingScope::all_accounts(),
    ))
}

fn client_policy(id: &str, plaintext: &str, enabled: bool) -> ClientPolicy {
    ClientPolicy::new(
        ClientApiKeyId::new(id).expect("client key ID"),
        PlaintextClientApiKey::new(plaintext).expect("plaintext client key"),
        account_scope(),
        enabled,
        RateLimits::unlimited(),
    )
}

fn snapshot() -> RuntimeSnapshot {
    RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![
            ProviderKind::new("openai").expect("provider"),
            ProviderKind::new("xai").expect("provider"),
        ],
        vec![
            model("openai", "gpt-5.5", capabilities()),
            model("xai", "grok-4.5", capabilities()),
        ],
        Vec::new(),
    )
    .expect("snapshot")
    .with_account_directory(account_directory())
    .with_model_mappings(BTreeMap::from([
        ("gpt-5.4".to_owned(), "gpt-5.5".to_owned()),
        ("grok-latest".to_owned(), "grok-4.5".to_owned()),
    ]))
}

#[test]
fn config_revision_should_reject_zero() {
    assert!(ConfigRevision::new(0).is_err());
}

#[test]
fn account_group_id_should_match_the_database_contract_exactly() {
    let valid = "grp_0123456789abcdef0123456789abcdef";
    assert_eq!(
        AccountGroupId::new(valid).expect("valid group ID").as_str(),
        valid
    );
    for invalid in [
        "0123456789abcdef0123456789abcdef",
        "grp_0123456789abcdef0123456789abcde",
        "grp_0123456789abcdef0123456789abcdef0",
        "grp_0123456789abcdef0123456789abcdeF",
        "grp_0123456789abcdef0123456789abcdeg",
    ] {
        assert!(
            AccountGroupId::new(invalid).is_err(),
            "invalid group ID was accepted: {invalid}"
        );
    }
}

#[test]
fn restricted_scope_should_use_enabled_group_union_and_preserve_all_bindings_for_history() {
    let enabled =
        AccountGroupId::new("grp_11111111111111111111111111111111").expect("enabled group");
    let disabled =
        AccountGroupId::new("grp_22222222222222222222222222222222").expect("disabled group");
    let provider = ProviderKind::new("openai").expect("provider");
    let allowed_account = ProviderAccountId::new("acct_allowed").expect("account");
    let disabled_only_account = ProviderAccountId::new("acct_disabled").expect("account");
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
        (
            allowed_account.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::from([enabled.clone()])),
        ),
        (
            disabled_only_account.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::from([disabled.clone()])),
        ),
    ])));
    let scope = FrozenAccountScope::new(
        directory,
        ClientRoutingScope::restricted(
            vec![
                RoutingGroupSnapshot::new(enabled.clone(), "Enabled".to_owned()),
                RoutingGroupSnapshot::new(disabled, "Disabled".to_owned()),
            ],
            BTreeSet::from([enabled]),
            BTreeSet::from([provider]),
        )
        .expect("restricted scope"),
    );

    assert!(scope.allows(&allowed_account));
    assert!(!scope.allows(&disabled_only_account));
    let history = scope.routing_snapshot();
    assert_eq!(history.kind(), AccountRoutingScopeKind::Groups);
    assert_eq!(history.groups_snapshot().len(), 2);
    assert_eq!(history.groups_snapshot()[1].name(), "Disabled");
}

#[test]
fn restricted_scope_with_no_enabled_group_should_fail_closed() {
    let group = AccountGroupId::new("grp_33333333333333333333333333333333").expect("group");
    let account = ProviderAccountId::new("acct_group_member").expect("account");
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
        account.clone(),
        RuntimeAccount::new(
            ProviderKind::new("openai").expect("provider"),
            BTreeSet::from([group.clone()]),
        ),
    )])));
    let scope = FrozenAccountScope::new(
        directory,
        ClientRoutingScope::restricted(
            vec![RoutingGroupSnapshot::new(group, "Disabled".to_owned())],
            BTreeSet::new(),
            BTreeSet::new(),
        )
        .expect("restricted scope"),
    );

    assert!(scope.provider_kinds().is_empty());
    assert!(!scope.allows(&account));
}

#[test]
fn snapshot_should_publish_only_enabled_plaintext_client_policies() {
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![ProviderKind::new("openai").expect("provider")],
        Vec::new(),
        vec![
            client_policy("key_enabled", "sk_enabled", true),
            client_policy("key_disabled", "sk_disabled", false),
        ],
    )
    .expect("snapshot");

    let policies = snapshot.client_policies().collect::<Vec<_>>();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].key_id().as_str(), "key_enabled");
    assert!(
        policies[0]
            .account_scope()
            .provider_kinds()
            .contains(&ProviderKind::new("openai").expect("provider"))
    );
}

#[test]
fn snapshot_should_reject_model_for_missing_provider() {
    let result = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        Vec::new(),
        vec![model("missing", "gpt-5.5", capabilities())],
        Vec::new(),
    );

    assert!(result.is_err());
}

#[test]
fn snapshot_should_reject_duplicate_provider_model() {
    let result = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![ProviderKind::new("openai").expect("provider")],
        vec![
            model("openai", "gpt-5.5", capabilities()),
            model("openai", "gpt-5.5", capabilities()),
        ],
        Vec::new(),
    );

    assert!(result.is_err());
}

#[test]
fn selected_provider_should_use_global_model_mapping() {
    let snapshot = snapshot();
    let plan = snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").expect("model"),
            &operation(),
            snapshot.all_account_scope(),
            &RoutingContext {
                required_provider: Some(ProviderKind::new("openai").expect("provider")),
                ..RoutingContext::default()
            },
        )
        .expect("plan");

    assert_eq!(plan.candidates().len(), 1);
    assert_eq!(plan.candidates()[0].provider().as_str(), "openai");
    assert_eq!(plan.candidates()[0].upstream_model().as_str(), "gpt-5.5");
}

#[test]
fn model_mapping_should_follow_a_bounded_alias_chain() {
    let snapshot = snapshot().with_model_mappings(BTreeMap::from([
        ("public-model".to_owned(), "compat-model".to_owned()),
        ("compat-model".to_owned(), "gpt-5.5".to_owned()),
    ]));

    assert_eq!(snapshot.mapped_model("public-model"), "gpt-5.5");
}

#[test]
fn cyclic_model_mapping_should_fall_back_to_the_original_name() {
    let snapshot = snapshot().with_model_mappings(BTreeMap::from([
        ("first".to_owned(), "second".to_owned()),
        ("second".to_owned(), "first".to_owned()),
    ]));

    assert_eq!(snapshot.mapped_model("first"), "first");
}

#[test]
fn known_provider_catalog_missing_mapped_model_should_be_filtered() {
    let snapshot = snapshot();
    let error = snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").expect("model"),
            &operation(),
            snapshot.all_account_scope(),
            &RoutingContext {
                required_provider: Some(ProviderKind::new("xai").expect("provider")),
                ..RoutingContext::default()
            },
        )
        .expect_err("known xAI catalog does not contain the mapped OpenAI model");

    assert!(matches!(
        error,
        gateway_core::error::RoutingError::NoCapableProvider { .. }
    ));
}

#[test]
fn unmapped_model_should_pass_through_unchanged() {
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![ProviderKind::new("openai").expect("provider")],
        Vec::new(),
        Vec::new(),
    )
    .expect("snapshot")
    .with_account_directory(account_directory());
    for requested in [
        "future-openai-model".to_owned(),
        format!("future-{}", "x".repeat(512)),
        "future\0model".to_owned(),
        "  future-openai-model  ".to_owned(),
    ] {
        let plan = snapshot
            .plan(
                &PublicModelId::from_client_wire(requested.clone()).expect("client model"),
                &operation(),
                snapshot.all_account_scope(),
                &RoutingContext {
                    required_provider: Some(ProviderKind::new("openai").expect("provider")),
                    ..RoutingContext::default()
                },
            )
            .expect("unknown model remains transparent");

        assert_eq!(plan.candidates()[0].upstream_model().as_str(), requested);
    }
}

#[test]
fn model_mapping_should_use_exact_client_keys() {
    let snapshot = snapshot().with_model_mappings(BTreeMap::from([(
        "  exact-alias  ".to_owned(),
        "gpt-5.5".to_owned(),
    )]));

    assert_eq!(snapshot.mapped_model("  exact-alias  "), "gpt-5.5");
    assert_eq!(snapshot.mapped_model("exact-alias"), "exact-alias");
}

#[test]
fn blocked_provider_should_be_filtered() {
    let snapshot = snapshot();
    let error = snapshot
        .plan(
            &PublicModelId::new("gpt-5.5").expect("model"),
            &operation(),
            snapshot.all_account_scope(),
            &RoutingContext {
                required_provider: Some(ProviderKind::new("openai").expect("provider")),
                blocked_providers: BTreeSet::from([ProviderKind::new("openai").expect("provider")]),
            },
        )
        .expect_err("blocked platform has no candidate");

    assert!(matches!(
        error,
        gateway_core::error::RoutingError::NoCapableProvider { .. }
    ));
}

#[test]
fn known_unsupported_operation_should_not_be_bypassed() {
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![ProviderKind::new("openai").expect("provider")],
        vec![model(
            "openai",
            "gpt-known-unsupported",
            ModelCapabilities::new(BTreeSet::new(), None),
        )],
        Vec::new(),
    )
    .expect("snapshot");

    assert!(
        snapshot
            .plan(
                &PublicModelId::new("gpt-known-unsupported").expect("model"),
                &operation(),
                snapshot.all_account_scope(),
                &RoutingContext {
                    required_provider: Some(ProviderKind::new("openai").expect("provider")),
                    ..RoutingContext::default()
                },
            )
            .is_err()
    );
}

#[test]
fn upstream_feature_validation_should_preserve_operation_and_limit_gates() {
    let capabilities =
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(16_000))
            .with_feature(
                Feature::Tools,
                gateway_core::routing::SupportLevel::Unsupported,
            )
            .with_upstream_feature_validation();
    let wire_features = CapabilityRequirements::new(OperationKind::Generate)
        .require(Feature::Tools)
        .require(Feature::JsonSchema);
    let oversized_output = CapabilityRequirements::new(OperationKind::Generate)
        .with_requested_output_tokens(Some(16_001));
    let unsupported_operation = CapabilityRequirements::new(OperationKind::Speech);

    assert_eq!(
        (
            capabilities.match_requirements(&wire_features),
            capabilities.match_requirements(&oversized_output),
            capabilities.match_requirements(&unsupported_operation),
        ),
        (Some(BTreeSet::new()), None, None)
    );
}

#[test]
fn public_catalog_should_include_discovered_models_and_aliases() {
    let models =
        snapshot().public_models_for_provider(&ProviderKind::new("openai").expect("provider"));
    let names = models
        .iter()
        .map(PublicModelId::as_str)
        .collect::<BTreeSet<_>>();

    assert_eq!(names, BTreeSet::from(["gpt-5.4", "gpt-5.5", "grok-latest"]));
}

#[test]
fn unknown_model_should_be_hidden_when_provider_catalog_is_available() {
    assert!(!snapshot().contains_public_model_for_provider(
        &PublicModelId::new("future-model").expect("model"),
        &ProviderKind::new("openai").expect("provider"),
    ));
}

#[test]
fn alias_should_only_be_available_from_a_provider_with_its_mapped_model() {
    let snapshot = snapshot();
    let alias = PublicModelId::new("grok-latest").expect("model");

    assert!(!snapshot.contains_public_model_for_provider(
        &alias,
        &ProviderKind::new("openai").expect("provider"),
    ));
    assert!(
        snapshot.contains_public_model_for_provider(
            &alias,
            &ProviderKind::new("xai").expect("provider"),
        )
    );
}
