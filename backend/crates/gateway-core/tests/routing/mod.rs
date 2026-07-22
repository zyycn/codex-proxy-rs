use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::time::Duration;

use gateway_core::engine::credential::{AccountSelectionPolicy, RotationStrategy};
use gateway_core::operation::{
    CapabilityRequirements, ContentPart, Feature, GenerateRequest, Message, MessageRole, Operation,
    OperationKind,
};
use gateway_core::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext,
    RuntimeSnapshot, UpstreamModelId,
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
    ModelCapabilities::new(
        BTreeSet::from([OperationKind::Generate]),
        128_000,
        Some(16_000),
    )
}

fn model(provider: &str, name: &str, capabilities: ModelCapabilities) -> ProviderModel {
    ProviderModel::new(
        ProviderKind::new(provider).expect("valid provider"),
        UpstreamModelId::new(name).expect("valid model"),
        capabilities,
    )
}

fn operation() -> Operation {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text("hello".to_owned())],
    )
    .expect("valid message");
    Operation::Generate(GenerateRequest::new(vec![message]).expect("valid request"))
}

fn client_policy(id: &str, plaintext: &str, enabled: bool) -> ClientPolicy {
    ClientPolicy::new(
        ClientApiKeyId::new(id).expect("client key ID"),
        PlaintextClientApiKey::new(plaintext).expect("plaintext client key"),
        ProviderKind::new("openai").expect("valid provider"),
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
    .with_provider_model_mappings(BTreeMap::from([
        (
            ProviderKind::new("openai").expect("provider"),
            BTreeMap::from([("gpt-5.4".to_owned(), "gpt-5.5".to_owned())]),
        ),
        (
            ProviderKind::new("xai").expect("provider"),
            BTreeMap::from([("grok-latest".to_owned(), "grok-4.5".to_owned())]),
        ),
    ]))
}

#[test]
fn config_revision_should_reject_zero() {
    assert!(ConfigRevision::new(0).is_err());
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
    assert_eq!(policies[0].provider_kind().as_str(), "openai");
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
fn platform_is_selected_before_model_mapping() {
    let snapshot = snapshot();
    let plan = snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").expect("model"),
            &operation(),
            &RoutingContext {
                provider_kind: Some(ProviderKind::new("openai").expect("provider")),
                ..RoutingContext::default()
            },
        )
        .expect("plan");

    assert_eq!(plan.candidates().len(), 1);
    assert_eq!(plan.candidates()[0].provider().as_str(), "openai");
    assert_eq!(plan.candidates()[0].upstream_model().as_str(), "gpt-5.5");
}

#[test]
fn mapping_must_not_cross_provider_boundary() {
    let snapshot = snapshot();
    let plan = snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").expect("model"),
            &operation(),
            &RoutingContext {
                provider_kind: Some(ProviderKind::new("xai").expect("provider")),
                ..RoutingContext::default()
            },
        )
        .expect("transparent plan");

    assert_eq!(plan.candidates()[0].provider().as_str(), "xai");
    assert_eq!(plan.candidates()[0].upstream_model().as_str(), "gpt-5.4");
}

#[test]
fn unmapped_model_should_pass_through_unchanged() {
    let snapshot = snapshot();
    let plan = snapshot
        .plan(
            &PublicModelId::new("future-openai-model").expect("model"),
            &operation(),
            &RoutingContext {
                provider_kind: Some(ProviderKind::new("openai").expect("provider")),
                ..RoutingContext::default()
            },
        )
        .expect("unknown model remains transparent");

    assert_eq!(
        plan.candidates()[0].upstream_model().as_str(),
        "future-openai-model"
    );
}

#[test]
fn blocked_provider_should_be_filtered() {
    let snapshot = snapshot();
    let error = snapshot
        .plan(
            &PublicModelId::new("gpt-5.5").expect("model"),
            &operation(),
            &RoutingContext {
                provider_kind: Some(ProviderKind::new("openai").expect("provider")),
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
            ModelCapabilities::new(BTreeSet::new(), 0, None),
        )],
        Vec::new(),
    )
    .expect("snapshot");

    assert!(
        snapshot
            .plan(
                &PublicModelId::new("gpt-known-unsupported").expect("model"),
                &operation(),
                &RoutingContext {
                    provider_kind: Some(ProviderKind::new("openai").expect("provider")),
                    ..RoutingContext::default()
                },
            )
            .is_err()
    );
}

#[test]
fn upstream_feature_validation_should_preserve_operation_and_limit_gates() {
    let capabilities = ModelCapabilities::new(
        BTreeSet::from([OperationKind::Generate]),
        128_000,
        Some(16_000),
    )
    .with_feature(
        Feature::Tools,
        gateway_core::routing::SupportLevel::Unsupported,
    )
    .with_upstream_feature_validation();
    let wire_features = CapabilityRequirements::new(OperationKind::Generate)
        .require(Feature::Tools)
        .require(Feature::JsonSchema);
    let oversized =
        CapabilityRequirements::new(OperationKind::Generate).with_minimum_context_tokens(128_001);
    let unsupported_operation = CapabilityRequirements::new(OperationKind::CompactConversation);

    assert_eq!(
        (
            capabilities.match_requirements(&wire_features),
            capabilities.match_requirements(&oversized),
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

    assert_eq!(names, BTreeSet::from(["gpt-5.4", "gpt-5.5"]));
}

#[test]
fn unknown_model_should_be_accepted_when_provider_is_registered() {
    assert!(snapshot().contains_public_model_for_provider(
        &PublicModelId::new("future-model").expect("model"),
        &ProviderKind::new("openai").expect("provider"),
    ));
}
