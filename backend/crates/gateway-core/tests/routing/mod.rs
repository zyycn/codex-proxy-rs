use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::time::Duration;

use gateway_core::engine::credential::{AccountSelectionPolicy, RotationStrategy};
use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, Operation, OperationKind,
};
use gateway_core::policy::{ClientApiKeyId, ClientPolicy, PlaintextClientApiKey, RateLimits};
use gateway_core::routing::{
    ConfigRevision, InstanceHealth, ModelCapabilities, ProviderInstance, ProviderInstanceId,
    ProviderKind, ProviderModel, PublicModelId, RoutingContext, RuntimeSnapshot, UpstreamModelId,
};

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

fn instance(id: &str, provider: &str) -> ProviderInstance {
    ProviderInstance::new(
        ProviderInstanceId::new(id).expect("valid instance"),
        ProviderKind::new(provider).expect("valid provider"),
        format!("https://{provider}.example.invalid"),
        true,
        InstanceHealth::Healthy,
    )
}

fn model(instance: &str, name: &str, capabilities: ModelCapabilities) -> ProviderModel {
    ProviderModel::new(
        ProviderInstanceId::new(instance).expect("valid instance"),
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
            instance("inst_openai", "openai"),
            instance("inst_xai", "xai"),
        ],
        vec![
            model("inst_openai", "gpt-5.5", capabilities()),
            model("inst_xai", "grok-4.5", capabilities()),
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
fn provider_instance_id_should_require_prefix() {
    assert!(ProviderInstanceId::new("missing").is_err());
}

#[test]
fn snapshot_should_publish_only_enabled_plaintext_client_policies() {
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![instance("inst_openai", "openai")],
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
fn snapshot_should_reject_model_for_missing_instance() {
    let result = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        Vec::new(),
        vec![model("inst_missing", "gpt-5.5", capabilities())],
        Vec::new(),
    );

    assert!(result.is_err());
}

#[test]
fn snapshot_should_reject_duplicate_instance_model() {
    let result = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![instance("inst_openai", "openai")],
        vec![
            model("inst_openai", "gpt-5.5", capabilities()),
            model("inst_openai", "gpt-5.5", capabilities()),
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
fn blocked_instance_should_be_filtered() {
    let snapshot = snapshot();
    let error = snapshot
        .plan(
            &PublicModelId::new("gpt-5.5").expect("model"),
            &operation(),
            &RoutingContext {
                provider_kind: Some(ProviderKind::new("openai").expect("provider")),
                blocked_instances: BTreeSet::from([
                    ProviderInstanceId::new("inst_openai").expect("instance")
                ]),
                ..RoutingContext::default()
            },
        )
        .expect_err("blocked platform has no candidate");

    assert!(matches!(
        error,
        gateway_core::error::RoutingError::NoCapableProvider { .. }
    ));
}

#[test]
fn known_unsupported_capability_should_not_be_bypassed() {
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        scheduling(),
        vec![instance("inst_openai", "openai")],
        vec![model(
            "inst_openai",
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
fn unknown_model_should_be_accepted_when_platform_has_instance() {
    assert!(snapshot().contains_public_model_for_provider(
        &PublicModelId::new("future-model").expect("model"),
        &ProviderKind::new("openai").expect("provider"),
    ));
}
