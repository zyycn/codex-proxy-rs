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
async fn adapted_catalog_freezes_adapter_accounts_and_preserves_the_active_generation() {
    use gateway_admin::model::plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy};
    let catalog = crate::support::contribution(
        Capability::ModelCatalog,
        vec![Stage::Registration],
        vec![],
        vec![],
    );
    let adapter = crate::support::contribution(
        Capability::UpstreamAdapter,
        vec![Stage::Upstream],
        vec!["openai".into()],
        vec!["openai".into()],
    );
    let (cache, store, runtime) =
        super::super::setup_with_contributions(Contributions::from([catalog, adapter])).await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].bindings = vec![PluginCapabilityBinding {
        contribution: "test.example.upstreamAdapter".into(),
        stage: "upstream".into(),
        order: 0,
        failure_policy: PluginFailurePolicy::Reject,
        client_key_ids: vec![],
        account_group_ids: vec![],
        provider_ids: vec!["openai".into()],
        models: vec!["shared-model".into()],
        event: None,
        identity_bindings: vec![],
    }];
    snapshot.instances[0].configuration = serde_json::json!({
        "upstream_registration":{"adapters":[{
            "id":"example","provider":"openai","base_url":"https://example.invalid/",
            "paths":[{"path":"responses","purpose":"inference"}], "authentication_kinds":["oauth"],
            "account_ids":["acct_b"], "models":["shared-model"], "transport":"http_sse","protocol":"openai"
        }]},
        "model_catalog":{"aliases":[{"id":"native-alias","provider":"openai","model":"native-model"}],"models":[{
            "id":"shared-model","adapter_id":"example","features":["tools"],"presentation":{"display_name":"Shared"}
        }]}
    });
    *store.snapshot.lock().unwrap() = snapshot.clone();
    let active = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(active.models().len(), 1);
    assert_eq!(
        active.models()[0].accounts.iter().next().unwrap().as_str(),
        "acct_b"
    );
    assert_eq!(active.model_aliases().len(), 1);
    assert_eq!(active.model_aliases()[0].id.as_str(), "native-alias");
    assert!(!active.models()[0].presentation.reasoning_summary());
    for summary in [None, Some(false), Some(true)] {
        let mut candidate = snapshot.clone();
        candidate.config_revision = Revision::new(2).unwrap();
        candidate.instances[0].revision = candidate.config_revision;
        let mut presentation = serde_json::json!({
            "display_name":"Shared", "default_reasoning_effort":"low",
            "supported_reasoning_efforts":["low","high"]
        });
        if let Some(summary) = summary {
            presentation["reasoning_summary"] = summary.into();
        }
        candidate.instances[0].configuration["model_catalog"]["models"][0]["presentation"] =
            presentation;
        let prepared = PluginPreparation::prepare(&runtime, candidate)
            .await
            .unwrap();
        let presentation = &prepared.models()[0].presentation;
        assert_eq!(presentation.default_reasoning_effort(), Some("low"));
        assert_eq!(presentation.supported_reasoning_efforts(), ["low", "high"]);
        assert_eq!(presentation.reasoning_summary(), summary.unwrap_or(false));
        assert!(active.is_ready());
    }
    for scenario in ["accounts", "model", "binding", "key", "group"] {
        let mut invalid = snapshot.clone();
        invalid.config_revision = Revision::new(2).unwrap();
        invalid.instances[0].revision = invalid.config_revision;
        let instance = &mut invalid.instances[0];
        match scenario {
            "accounts" => {
                instance.configuration["upstream_registration"]["adapters"][0]["account_ids"] =
                    serde_json::json!([])
            }
            "model" => {
                instance.configuration["upstream_registration"]["adapters"][0]["models"] =
                    serde_json::json!(["other"])
            }
            "binding" => instance.bindings[0].models = vec!["other".into()],
            "key" => instance.bindings[0].client_key_ids = vec!["key_one".into()],
            "group" => {
                instance.bindings[0].account_group_ids =
                    vec!["grp_0123456789abcdef0123456789abcdef".into()]
            }
            _ => unreachable!(),
        }
        let error = PluginPreparation::prepare(&runtime, invalid)
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "适配模型必须关联同实例的静态账号适配器",
            "{scenario}"
        );
        assert!(active.is_ready());
    }
    for conflict in [
        None,
        Some("accounts"),
        Some("presentation"),
        Some("features"),
        Some("alias"),
    ] {
        let mut candidate = snapshot.clone();
        candidate.config_revision = Revision::new(2).unwrap();
        candidate.instances[0].revision = candidate.config_revision;
        let config = &mut candidate.instances[0].configuration;
        let mut adapter = config["upstream_registration"]["adapters"][0].clone();
        adapter["id"] = "second".into();
        adapter["account_ids"] = serde_json::json!([if conflict == Some("accounts") {
            "acct_b"
        } else {
            "acct_a"
        }]);
        config["upstream_registration"]["adapters"]
            .as_array_mut()
            .unwrap()
            .push(adapter);
        let mut model = config["model_catalog"]["models"][0].clone();
        model["adapter_id"] = "second".into();
        if conflict == Some("presentation") {
            model["presentation"]["display_name"] = "Other".into();
        }
        if conflict == Some("features") {
            model["features"] = serde_json::json!([]);
        }
        config["model_catalog"]["models"]
            .as_array_mut()
            .unwrap()
            .push(model);
        if conflict == Some("alias") {
            config["model_catalog"]["aliases"] =
                serde_json::json!([{"id":"shared-model","provider":"openai","model":"target"}]);
        }
        let result = PluginPreparation::prepare(&runtime, candidate).await;
        if conflict.is_some() {
            assert_eq!(result.unwrap_err().message(), "适配模型目录声明冲突");
        } else {
            let prepared = result.unwrap();
            assert_eq!(prepared.models().len(), 2);
            assert!(
                prepared.models()[0]
                    .accounts
                    .is_disjoint(&prepared.models()[1].accounts)
            );
        }
        assert!(active.is_ready());
    }
    snapshot.config_revision = Revision::new(2).unwrap();
    snapshot.instances[0].revision = snapshot.config_revision;
    snapshot.instances[0].configuration = serde_json::json!({"startup":"fail"});
    *store.snapshot.lock().unwrap() = snapshot.clone();
    assert!(
        ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
            .await
            .is_err()
    );
    snapshot.instances[0].enabled = false;
    *store.snapshot.lock().unwrap() = snapshot;
    let disabled = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
        .await
        .unwrap();
    assert!(disabled.models().is_empty());
    assert_eq!(active.models().len(), 1);
    drop(disabled);
    drop(active);
    runtime.shutdown().await;
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn adapted_catalog_requires_its_registered_adapter() {
    let catalog = crate::support::contribution(
        Capability::ModelCatalog,
        vec![Stage::Registration],
        vec![],
        vec![],
    );
    let (cache, store, runtime) =
        super::super::setup_with_contributions(Contributions::from([catalog])).await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "model_catalog": {"aliases":[], "models":[{
            "id":"shared-model", "adapter_id":"missing", "features":[],
            "max_output_tokens":null, "presentation":{}
        }]}
    });
    let error = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .unwrap_err();
    assert_eq!(error.message(), "适配模型必须关联同实例的静态账号适配器");
    runtime.shutdown().await;
    super::super::wait_until_empty(cache.path()).await;
}

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
            "model_catalog":{"aliases":[{"id":"fixture-alias","provider":"openai","model":"fixture-upstream"}],"models":[]}
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
        "model_catalog": {"aliases": [{"id": "my-model", "provider": "openai", "model": "upstream-model"}], "models": []}
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
    for aliases in [
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
            serde_json::json!({"model_catalog":{"aliases":aliases,"models":[]}});
        let expected = aliases.as_array().unwrap().len();
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
        "model_catalog":{"aliases":[{"id":"alias","provider":"openai","model":"target"}],"models":[]}
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
