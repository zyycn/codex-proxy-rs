//! 验证完整快照准备、能力目录与失败实例恢复

use gateway_admin::model::Revision;
use gateway_admin::model::plugins::instances::PluginInstanceRuntimeStatus;
use gateway_admin::ports::plugins::PluginPreparation;
use gateway_core::routing::ConfigRevision;
use gateway_core::routing::extensions::ExtensionPreparationPort;
use gateway_plugin_sdk::Capability;
use gateway_plugin_sdk::Contributions;
use gateway_plugin_sdk::Stage;

#[tokio::test]
async fn initial_registration_http_uses_the_prepared_settings_without_a_publication() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    for limit in [63, 64] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(u64::from(limit == 64))
            .mount(&server)
            .await;
        let (cache, store, runtime) =
            super::super::setup_with_contributions(Contributions::from([
                crate::support::contribution(
                    Capability::ModelCatalog,
                    vec![Stage::Registration],
                    vec![],
                    vec![],
                ),
            ]))
            .await;
        store.snapshot.lock().unwrap().instances[0].configuration = serde_json::json!({
            "registration_http": {"params": {"method":"POST", "url":server.uri(), "headers":[]}, "body":"x".repeat(64)},
            "model_catalog":{"models":[{"id":"fixture-alias","provider":"openai","model":"fixture-upstream"}]}
        });
        let prepared = ExtensionPreparationPort::prepare_with_request_body_limit(
            &runtime,
            ConfigRevision::new(1).unwrap(),
            limit,
        )
        .await
        .unwrap();
        assert_eq!(prepared.model_aliases().len(), usize::from(limit == 64));
        drop(prepared);
        runtime.shutdown().await;
        super::super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn model_catalog_is_frozen_without_entering_the_request_chain() {
    let (cache, store, runtime) = super::super::setup_with_contributions(Contributions::from([
        crate::support::contribution(
            Capability::ModelCatalog,
            vec![Stage::Registration],
            vec![],
            vec![],
        ),
    ]))
    .await;
    store.snapshot.lock().unwrap().instances[0].configuration = serde_json::json!({
        "model_catalog": {"models": [{"id": "my-model", "provider": "openai", "model": "upstream-model"}]}
    });
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(generation.model_aliases().len(), 1);
    assert_eq!(generation.model_aliases()[0].id.as_str(), "my-model");
    assert_eq!(
        generation.model_aliases()[0].target.as_str(),
        "upstream-model"
    );
    assert!(runtime.policy_registry().resolve(&generation).is_none());
    assert!(
        runtime
            .execution_registry()
            .middleware(&generation)
            .is_none()
    );
    let reused = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(reused.id(), generation.id());
    assert_eq!(reused.model_aliases(), generation.model_aliases());
    {
        let mut snapshot = store.snapshot.lock().unwrap();
        snapshot.config_revision = Revision::new(2).unwrap();
        snapshot.instances[0].revision = Revision::new(2).unwrap();
        snapshot.instances[0].configuration = serde_json::json!({"startup":"fail"});
    }
    assert!(
        ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
            .await
            .is_err(),
        "failed recovery must not publish a catalog with silently removed aliases"
    );
    assert!(generation.is_ready());
    assert_eq!(generation.model_aliases()[0].id.as_str(), "my-model");
    {
        let mut snapshot = store.snapshot.lock().unwrap();
        snapshot.config_revision = Revision::new(2).unwrap();
        snapshot.instances[0].enabled = false;
    }
    let disabled = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
        .await
        .unwrap();
    assert!(disabled.model_aliases().is_empty());
    assert_eq!(generation.model_aliases().len(), 1);
    drop(disabled);
    drop(reused);
    drop(generation);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn model_catalog_prepares_plugin_aliases_without_semantic_validation() {
    for models in [
        serde_json::json!([{"id":"alias","provider":"custom","model":"target"}]),
        serde_json::json!([{"id":"alias","provider":"openai","model":"alias"}]),
        serde_json::json!([
            {"id":"alias","provider":"openai","model":"target"},
            {"id":"alias","provider":"xai","model":"target"}
        ]),
        serde_json::json!([{"id":"alias","provider":"openai","model":"target","capabilities":[]}]),
    ] {
        let (cache, store, runtime) =
            super::super::setup_with_contributions(Contributions::from([
                crate::support::contribution(
                    Capability::ModelCatalog,
                    vec![Stage::Registration],
                    vec![],
                    vec![],
                ),
            ]))
            .await;
        let mut snapshot = store.snapshot.lock().unwrap().clone();
        snapshot.instances[0].configuration =
            serde_json::json!({"model_catalog":{"models":models}});
        let expected = models.as_array().unwrap().len();
        let prepared = PluginPreparation::prepare(&runtime, snapshot)
            .await
            .unwrap();
        assert_eq!(prepared.model_aliases().len(), expected);
        drop(prepared);
        runtime.shutdown().await;
        super::super::wait_until_empty(cache.path()).await;
    }

    let (cache, store, runtime) = super::super::setup_with_contributions(Contributions::from([
        crate::support::contribution(
            Capability::ModelCatalog,
            vec![Stage::Registration],
            vec![],
            vec![],
        ),
    ]))
    .await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "model_catalog":{"models":[{"id":"alias","provider":"openai","model":"target"}]}
    });
    let mut other = snapshot.instances[0].clone();
    other.id = "instance-two".into();
    snapshot.instances.push(other);
    let prepared = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .unwrap();
    assert_eq!(prepared.model_aliases().len(), 2);
    assert_eq!(prepared.model_aliases()[0].owner, "instance-one");
    assert_eq!(prepared.model_aliases()[1].owner, "instance-two");
    drop(prepared);
    runtime.shutdown().await;
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn failed_candidate_leaves_the_previous_generation_ready() {
    let (cache, store, runtime) = super::super::setup().await;
    let snapshot = store.snapshot.lock().unwrap().clone();
    let active = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();
    let mut candidate = snapshot;
    candidate.instances[0].configuration = serde_json::json!({"startup":"fail"});
    assert!(
        PluginPreparation::prepare(&runtime, candidate)
            .await
            .is_err()
    );
    assert!(active.is_ready());
    drop(active);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn disabled_missing_artifacts_do_not_block_the_remaining_generation_or_offline_recovery() {
    let (cache, store, runtime) = super::super::setup().await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    let mut missing = snapshot.instances[0].clone();
    missing.id = "missing".into();
    missing.enabled = false;
    missing.artifact_sha256 = "0".repeat(64);
    snapshot.instances.push(missing);
    let active = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .unwrap();
    assert!(active.is_ready());
    drop(active);
    super::super::wait_until_empty(cache.path()).await;
    let restored = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert!(restored.is_ready());
    drop(restored);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn preparation_cannot_mix_a_newer_persisted_revision_with_an_older_snapshot() {
    let (_cache, _store, runtime) = super::super::setup().await;
    assert!(
        ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn restoring_a_failed_plugin_keeps_other_instances_ready_and_reports_only_its_failure() {
    let (cache, store, runtime) = super::super::setup().await;
    let snapshot = {
        let mut snapshot = store.snapshot.lock().unwrap();
        let mut failed = snapshot.instances[0].clone();
        failed.id = "failed-plugin".into();
        failed.configuration = serde_json::json!({"startup":"fail"});
        snapshot.instances.insert(0, failed);
        snapshot.clone()
    };
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .expect("one failed plugin must not stop restoration");
    assert!(generation.can_serve());
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(1),
        Some(&generation),
    )
    .await
    .unwrap();
    assert_eq!(
        diagnostics["failed-plugin"].status,
        PluginInstanceRuntimeStatus::PreparationFailed
    );
    assert!(diagnostics["failed-plugin"].failure.is_some());
    assert_eq!(
        diagnostics["instance-one"].status,
        PluginInstanceRuntimeStatus::Running
    );
    let reused = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(
        generation.id(),
        reused.id(),
        "quarantined failures do not restart on every request"
    );
    drop(reused);
    drop(generation);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn an_existing_failed_plugin_does_not_prevent_editing_an_unrelated_instance() {
    let (cache, store, runtime) = super::super::setup().await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    let mut failed = snapshot.instances[0].clone();
    failed.id = "failed-plugin".into();
    failed.configuration = serde_json::json!({"startup":"fail"});
    snapshot.instances.push(failed);
    snapshot.config_revision = Revision::new(2).unwrap();
    snapshot.instances[0].revision = snapshot.config_revision;
    let candidate = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .expect("only the instance being changed must successfully prepare");
    assert!(candidate.can_serve());
    drop(candidate);
    super::super::wait_until_empty(cache.path()).await;
}
