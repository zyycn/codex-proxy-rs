use gateway_admin::{
    model::{
        Revision,
        plugins::instances::{
            PluginCapabilityBinding, PluginFailurePolicy, PluginInstanceRuntimeStatus,
        },
    },
    ports::plugins::PluginPreparation,
};
use gateway_core::{routing::ConfigRevision, runtime::extensions::ExtensionPreparationPort};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use std::{num::NonZeroUsize, path::Path, time::Duration};

async fn wait_until_unready(reference: &gateway_core::runtime::extensions::ExtensionSetReference) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while reference.is_ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker exit observed");
}

fn startup_count(marker: &Path) -> usize {
    std::fs::read_to_string(marker)
        .map(|content| content.lines().count())
        .unwrap_or_default()
}

#[tokio::test]
async fn model_catalog_is_frozen_without_entering_the_request_chain() {
    let (cache, store, runtime) =
        super::setup_with_contributions(Contributions::from([crate::support::contribution(
            Capability::ModelCatalog,
            vec![Stage::Registration],
            vec![],
            vec![],
        )]))
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
    assert!(runtime.middleware_registry().resolve(&generation).is_none());
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
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn model_catalog_rejects_invalid_registrations_and_cross_instance_conflicts() {
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
            super::setup_with_contributions(Contributions::from([crate::support::contribution(
                Capability::ModelCatalog,
                vec![Stage::Registration],
                vec![],
                vec![],
            )]))
            .await;
        let mut snapshot = store.snapshot.lock().unwrap().clone();
        snapshot.instances[0].configuration =
            serde_json::json!({"model_catalog":{"models":models}});
        assert!(
            PluginPreparation::prepare(&runtime, snapshot)
                .await
                .is_err()
        );
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }

    let (cache, store, runtime) =
        super::setup_with_contributions(Contributions::from([crate::support::contribution(
            Capability::ModelCatalog,
            vec![Stage::Registration],
            vec![],
            vec![],
        )]))
        .await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "model_catalog":{"models":[{"id":"alias","provider":"openai","model":"target"}]}
    });
    let mut other = snapshot.instances[0].clone();
    other.id = "instance-two".into();
    snapshot.instances.push(other);
    let error = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("instance-one"));
    assert!(error.to_string().contains("instance-two"));
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn runtime_shutdown_reaps_live_generation_and_rejects_late_preparation() {
    let (cache, store, runtime) = super::setup().await;
    let published = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert!(published.is_ready());
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);

    runtime.shutdown().await;
    assert!(!published.is_ready());
    super::wait_until_empty(cache.path()).await;
    assert!(
        ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
            .await
            .is_err()
    );
    runtime.shutdown().await;

    drop(published);
    drop(store);
}

#[tokio::test]
async fn management_needs_no_binding_and_rejects_stale_execution_identity_configuration() {
    let (_cache, store, runtime) =
        super::setup_with_contributions(Contributions::from([crate::support::contribution(
            Capability::Management,
            vec![Stage::Management],
            Vec::new(),
            Vec::new(),
        )]))
        .await;
    let mut instance = store.snapshot.lock().unwrap().instances[0].clone();
    assert!(
        PluginPreparation::validate(&runtime, instance.clone())
            .await
            .is_ok()
    );
    instance.bindings = vec![PluginCapabilityBinding {
        contribution: "test.example.management".into(),
        stage: "management".into(),
        order: 0,
        failure_policy: PluginFailurePolicy::Reject,
        client_key_ids: vec!["key-one".into(), "key-two".into()],
        account_group_ids: Vec::new(),
        provider_ids: Vec::new(),
        models: Vec::new(),
        identity_bindings: Vec::new(),
    }];
    assert!(
        PluginPreparation::validate(&runtime, instance.clone())
            .await
            .is_err()
    );
    instance.enabled = false;
    assert!(
        PluginPreparation::validate(&runtime, instance)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn registration_with_a_changed_contribution_id_is_rejected() {
    let (cache, store, runtime) =
        super::setup_with_contributions(Contributions::from([crate::support::contribution(
            Capability::Usage,
            vec![Stage::Observation],
            Vec::new(),
            Vec::new(),
        )]))
        .await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    let candidate = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("the declared contribution prepares before its registration id changes");
    drop(candidate);
    super::wait_until_empty(cache.path()).await;

    snapshot.instances[0].configuration = serde_json::json!({"registration_mismatch":true});
    let error = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .expect_err("a changed registration contribution id must be rejected");
    assert!(
        error.to_string().contains("插件注册结果与清单不符"),
        "unexpected preparation failure: {error}"
    );
}

#[tokio::test]
async fn candidate_is_reused_after_commit_and_released_when_no_generation_owner_remains() {
    let (cache, store, runtime) = super::setup().await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration =
        serde_json::from_str(r#"{"z":1,"a":{"z":2,"a":3}}"#).unwrap();
    let candidate = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();
    assert!(
        runtime.observer_registry().resolve(&candidate).is_none(),
        "没有观察绑定时不应编译派发计划"
    );
    snapshot.instances[0].configuration =
        serde_json::from_str(r#"{"a":{"a":3,"z":2},"z":1}"#).unwrap();
    snapshot.config_revision = Revision::new(2).unwrap();
    *store.snapshot.lock().unwrap() = snapshot;
    let published = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
        .await
        .unwrap();
    assert_eq!(candidate.id(), published.id());
    assert!(published.is_ready());
    drop(candidate);
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
    drop(published);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn diagnostics_distinguish_running_failed_preparation_and_draining_generations() {
    let (cache, store, runtime) = super::setup().await;
    let snapshot = store.snapshot.lock().unwrap().clone();
    let active = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();

    let running = PluginPreparation::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(snapshot.config_revision.get()),
        Some(&active),
    )
    .await
    .expect("runtime diagnostics");
    let running = &running["instance-one"];
    assert_eq!(running.status, PluginInstanceRuntimeStatus::Running);
    assert_eq!(running.actual_revision, Some(1));
    assert_eq!(
        running.actual_artifact_sha256.as_deref(),
        Some(snapshot.instances[0].artifact_sha256.as_str())
    );
    assert!(running.failure.is_none());

    let mut failed = snapshot.clone();
    failed.config_revision = Revision::new(2).unwrap();
    failed.instances[0].revision = Revision::new(2).unwrap();
    failed.instances[0].configuration = serde_json::json!({"startup":"fail"});
    assert!(
        PluginPreparation::prepare(&runtime, failed.clone())
            .await
            .is_err()
    );
    let diagnostics = PluginPreparation::runtime_diagnostics(
        &runtime,
        &failed,
        Some(snapshot.config_revision.get()),
        Some(&active),
    )
    .await
    .expect("failed preparation diagnostics");
    let failed_runtime = &diagnostics["instance-one"];
    assert_eq!(
        failed_runtime.status,
        PluginInstanceRuntimeStatus::PreparationFailed
    );
    assert!(failed_runtime.failure.is_some());
    assert_eq!(failed_runtime.actual_revision, Some(1));

    let mut disabled = failed;
    disabled.config_revision = Revision::new(3).unwrap();
    disabled.instances[0].revision = Revision::new(3).unwrap();
    disabled.instances[0].enabled = false;
    let diagnostics = PluginPreparation::runtime_diagnostics(
        &runtime,
        &disabled,
        Some(snapshot.config_revision.get()),
        Some(&active),
    )
    .await
    .expect("draining diagnostics");
    let draining = &diagnostics["instance-one"];
    assert_eq!(draining.status, PluginInstanceRuntimeStatus::Draining);
    assert_eq!(draining.draining_revisions, [1]);

    drop(active);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn preparing_diagnostics_never_wait_for_the_serialized_prepare_io() {
    let (cache, store, runtime) = super::setup().await;
    let source = store.snapshot.lock().unwrap().clone();
    let mut candidate = source.clone();
    candidate.config_revision = Revision::new(2).unwrap();
    candidate.instances[0].revision = Revision::new(2).unwrap();
    candidate.instances[0].configuration = serde_json::json!({"startup_delay_ms":400});
    let diagnostic_snapshot = candidate.clone();
    let mut preparation = Box::pin(PluginPreparation::prepare(&runtime, candidate));
    assert!(futures::poll!(preparation.as_mut()).is_pending());

    let diagnostics = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        PluginPreparation::runtime_diagnostics(&runtime, &diagnostic_snapshot, None, None),
    )
    .await
    .expect("diagnostics must not wait for plugin startup")
    .expect("runtime diagnostics");
    assert_eq!(
        diagnostics["instance-one"].status,
        PluginInstanceRuntimeStatus::Preparing
    );

    let candidate = preparation.await.expect("delayed candidate");
    drop(candidate);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn diagnostics_report_a_published_process_fault_without_plugin_details() {
    let (cache, store, runtime) = super::setup().await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({"exit_after_ready_ms":20});
    let active = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("candidate starts before the controlled exit");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while active.is_ready() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker exit observed");

    let diagnostics = PluginPreparation::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(snapshot.config_revision.get()),
        Some(&active),
    )
    .await
    .expect("runtime diagnostics");
    let fault = &diagnostics["instance-one"];
    assert_eq!(fault.status, PluginInstanceRuntimeStatus::Faulted);
    assert_eq!(
        fault.failure.as_ref().map(|failure| failure.code.as_str()),
        Some("closed")
    );
    assert_eq!(
        fault
            .failure
            .as_ref()
            .map(|failure| failure.message.as_str()),
        Some("插件进程或传输已停止")
    );

    drop(active);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_caps_short_lived_crashes_and_a_new_revision_recovers() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_secs(1),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("starts.jsonl");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker": marker,
        "exit_after_ready_ms": 20,
    });
    let mut active = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("first incarnation");
    for expected_start in 2..=3 {
        wait_until_unready(&active).await;
        let replacement = PluginPreparation::prepare(&runtime, snapshot.clone())
            .await
            .expect("failure budget still permits a restart");
        drop(active);
        active = replacement;
        assert_eq!(startup_count(&marker), expected_start);
    }
    wait_until_unready(&active).await;
    assert!(
        PluginPreparation::prepare(&runtime, snapshot.clone())
            .await
            .is_err(),
        "the fourth incarnation must not start"
    );
    assert_eq!(startup_count(&marker), 3);

    let diagnostics = PluginPreparation::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(snapshot.config_revision.get()),
        Some(&active),
    )
    .await
    .expect("runtime diagnostics");
    assert_eq!(
        diagnostics["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("restart_circuit_open")
    );

    let mut disabled = snapshot;
    disabled.config_revision = Revision::new(2).unwrap();
    disabled.instances[0].revision = Revision::new(2).unwrap();
    disabled.instances[0].enabled = false;
    let disabled_generation = PluginPreparation::prepare(&runtime, disabled.clone())
        .await
        .expect("disabling the instance does not reuse its open circuit");
    assert_eq!(startup_count(&marker), 3);

    let mut repaired = disabled;
    repaired.config_revision = Revision::new(3).unwrap();
    repaired.instances[0].revision = Revision::new(3).unwrap();
    repaired.instances[0].enabled = true;
    repaired.instances[0].configuration = serde_json::json!({"startup_marker":marker});
    let replacement = PluginPreparation::prepare(&runtime, repaired)
        .await
        .expect("a new instance revision has an independent failure budget");
    assert!(replacement.is_ready());
    assert_eq!(startup_count(&marker), 4);

    drop(active);
    drop(disabled_generation);
    drop(replacement);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_resets_after_a_stable_incarnation() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_millis(80),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("stable-reset-starts.jsonl");
    let exit = controls.path().join("stable-exit");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker":marker,
        "startup_failures":[true,true,false,true,true],
        "exit_after_ready_signals":[null,null,exit],
    });
    // 握手前失败的有效运行时间固定为零；不能用短 sleep 假定宿主一定及时观察到退出。
    for _ in 0..2 {
        assert!(
            PluginPreparation::prepare(&runtime, snapshot.clone())
                .await
                .is_err()
        );
    }
    let active = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("the third incarnation starts before the circuit opens");
    assert!(active.is_ready());
    // 明确等到稳定窗口之后才允许第三个进程退出；调度变慢不会把短命进程误变为稳定进程。
    tokio::time::sleep(restart_circuit.stability_window).await;
    tokio::fs::write(exit, b"exit").await.unwrap();
    wait_until_unready(&active).await;
    for expected_start in 4..=5 {
        assert!(
            PluginPreparation::prepare(&runtime, snapshot.clone())
                .await
                .is_err()
        );
        assert_eq!(startup_count(&marker), expected_start);
    }
    assert!(
        PluginPreparation::prepare(&runtime, snapshot)
            .await
            .is_err(),
        "three new failures after the stable incarnation reopen the circuit"
    );
    assert_eq!(startup_count(&marker), 5);

    drop(active);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_ignores_planned_generation_shutdown() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::MIN,
        stability_window: Duration::from_secs(60),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("planned-shutdown-starts.jsonl");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({"startup_marker":marker});
    let first = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("first generation");
    let instance = &snapshot.instances[0];
    PluginPreparation::quiesce_instance(
        &runtime,
        &instance.id,
        &instance.artifact_sha256,
        instance.revision,
    )
    .await;
    assert!(!first.is_ready());

    let second = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .expect("planned shutdown must not open the one-failure circuit");
    assert!(second.is_ready());
    assert_eq!(startup_count(&marker), 2);

    drop(first);
    drop(second);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_counts_exit_before_the_candidate_is_indexed() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_secs(1),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("registration-exits.jsonl");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker":marker,
        "exit_during_registration":true,
    });
    for _ in 0..3 {
        assert!(
            PluginPreparation::prepare(&runtime, snapshot.clone())
                .await
                .is_err()
        );
    }
    assert_eq!(startup_count(&marker), 3);
    assert!(
        PluginPreparation::prepare(&runtime, snapshot.clone())
            .await
            .is_err()
    );
    assert_eq!(
        startup_count(&marker),
        3,
        "open circuit must gate process spawn"
    );

    let diagnostics = PluginPreparation::runtime_diagnostics(&runtime, &snapshot, None, None)
        .await
        .expect("runtime diagnostics");
    assert_eq!(
        diagnostics["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("restart_circuit_open")
    );
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_counts_a_started_process_that_exits_during_handshake() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(2).unwrap(),
        stability_window: Duration::from_millis(10),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("handshake-exits.jsonl");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker":marker,
        "startup":"fail",
        "startup_fail_delay_ms":50,
    });
    for _ in 0..2 {
        assert!(
            PluginPreparation::prepare(&runtime, snapshot.clone())
                .await
                .is_err()
        );
    }
    assert_eq!(startup_count(&marker), 2);
    assert!(
        PluginPreparation::prepare(&runtime, snapshot)
            .await
            .is_err()
    );
    assert_eq!(
        startup_count(&marker),
        2,
        "host-side gating must happen before another process spawn"
    );
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_does_not_let_an_older_generation_clear_newer_failures() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_millis(50),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("overlapping-generations.jsonl");
    let old_exit = controls.path().join("old-generation-exit");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker":marker,
        "exit_after_ready_delays_ms":[null,15,15,15],
        "exit_after_ready_signals":[old_exit,null,null,null],
    });
    let mut other = snapshot.instances[0].clone();
    other.id = "other-instance".into();
    other.name = "Other".into();
    other.configuration = serde_json::json!({});
    snapshot.instances.push(other);
    let published = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("published generation");
    assert!(published.is_ready());

    let mut failed_candidate = None;
    for revision in 2..=4 {
        snapshot.config_revision = Revision::new(revision).unwrap();
        snapshot.instances[1].revision = Revision::new(revision).unwrap();
        snapshot.instances[1].configuration = serde_json::json!({"revision":revision});
        let candidate = PluginPreparation::prepare(&runtime, snapshot.clone())
            .await
            .expect("newer candidate starts before its controlled exit");
        wait_until_unready(&candidate).await;
        drop(failed_candidate.replace(candidate));
        assert!(published.is_ready(), "the older generation remains healthy");
    }
    drop(failed_candidate.take());
    std::fs::write(&old_exit, b"exit").unwrap();
    wait_until_unready(&published).await;
    snapshot.config_revision = Revision::new(5).unwrap();
    snapshot.instances[1].revision = Revision::new(5).unwrap();
    snapshot.instances[1].configuration = serde_json::json!({"revision":5});
    let circuit = PluginPreparation::runtime_diagnostics(&runtime, &snapshot, None, None)
        .await
        .unwrap();
    assert_eq!(
        circuit["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("restart_circuit_open"),
    );
    let recovered = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("an older failed instance is isolated during unrelated recovery");
    let diagnostics =
        PluginPreparation::runtime_diagnostics(&runtime, &snapshot, Some(5), Some(&recovered))
            .await
            .unwrap();
    assert_eq!(
        diagnostics["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("unavailable"),
        "the older healthy session must not erase newer failures for the same identity"
    );
    assert_eq!(startup_count(&marker), 4);

    drop(recovered);
    drop(published);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_restores_an_older_failure_after_candidate_shutdown() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::MIN,
        stability_window: Duration::from_secs(1),
    };
    let (cache, store, runtime) = super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("abandoned-candidate.jsonl");
    let old_exit = controls.path().join("abandoned-candidate-old-exit");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker":marker,
        "exit_after_ready_delays_ms":[null,null],
        "exit_after_ready_signals":[old_exit,null],
    });
    let mut other = snapshot.instances[0].clone();
    other.id = "other-instance".into();
    other.name = "Other".into();
    other.configuration = serde_json::json!({});
    snapshot.instances.push(other);
    let published = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("published generation");

    snapshot.config_revision = Revision::new(2).unwrap();
    snapshot.instances[1].revision = Revision::new(2).unwrap();
    snapshot.instances[1].configuration = serde_json::json!({"revision":2});
    let candidate = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("healthy replacement candidate");
    std::fs::write(&old_exit, b"exit").unwrap();
    wait_until_unready(&published).await;
    assert!(candidate.is_ready());
    drop(candidate);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let diagnostics = PluginPreparation::runtime_diagnostics(
                &runtime,
                &snapshot,
                Some(1),
                Some(&published),
            )
            .await
            .expect("runtime diagnostics");
            if diagnostics["instance-one"]
                .failure
                .as_ref()
                .is_some_and(|failure| failure.code == "restart_circuit_open")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("older failure is restored after candidate shutdown");

    snapshot.config_revision = Revision::new(3).unwrap();
    snapshot.instances[1].revision = Revision::new(3).unwrap();
    snapshot.instances[1].configuration = serde_json::json!({"revision":3});
    let recovered = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("an older failed instance is isolated during unrelated recovery");
    let diagnostics =
        PluginPreparation::runtime_diagnostics(&runtime, &snapshot, Some(3), Some(&recovered))
            .await
            .unwrap();
    assert_eq!(
        diagnostics["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("unavailable"),
    );
    assert_eq!(startup_count(&marker), 2);

    drop(recovered);
    drop(published);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn failed_candidate_leaves_the_previous_generation_ready() {
    let (cache, store, runtime) = super::setup().await;
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
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn disabled_missing_artifacts_do_not_block_the_remaining_generation_or_offline_recovery() {
    let (cache, store, runtime) = super::setup().await;
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
    super::wait_until_empty(cache.path()).await;
    let restored = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert!(restored.is_ready());
    drop(restored);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn preparation_cannot_mix_a_newer_persisted_revision_with_an_older_snapshot() {
    let (_cache, _store, runtime) = super::setup().await;
    assert!(
        ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(2).unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn restoring_a_failed_plugin_keeps_other_instances_ready_and_reports_only_its_failure() {
    let (cache, store, runtime) = super::setup().await;
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
    let diagnostics =
        PluginPreparation::runtime_diagnostics(&runtime, &snapshot, Some(1), Some(&generation))
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
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn a_published_process_crash_preserves_the_serving_snapshot_and_other_plugin_status() {
    let (cache, store, runtime) = super::setup().await;
    let snapshot = {
        let mut snapshot = store.snapshot.lock().unwrap();
        let mut failed = snapshot.instances[0].clone();
        failed.id = "crashing-plugin".into();
        failed.configuration = serde_json::json!({"exit_after_ready_ms":100});
        snapshot.instances.push(failed);
        snapshot.clone()
    };
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    wait_until_unready(&generation).await;
    assert!(generation.can_serve());
    assert!(!generation.is_ready());
    let diagnostics =
        PluginPreparation::runtime_diagnostics(&runtime, &snapshot, Some(1), Some(&generation))
            .await
            .unwrap();
    assert_eq!(
        diagnostics["instance-one"].status,
        PluginInstanceRuntimeStatus::Running
    );
    assert_eq!(
        diagnostics["crashing-plugin"].status,
        PluginInstanceRuntimeStatus::Faulted
    );
    drop(generation);
    super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn an_incompatible_host_quarantines_the_plugin_with_a_specific_version_error() {
    use std::sync::Arc;
    let (cache, store, _) = super::setup().await;
    let runtime = gateway_plugin_runtime::PluginRuntime::new(
        store.clone(),
        store.clone(),
        gateway_plugin_runtime::PluginRuntimeConfig {
            cache_directory: cache.path().to_owned(),
            host_version: "3.0.0".parse().unwrap(),
            package_limits: Default::default(),
            rpc_limits: Default::default(),
            restart_circuit: Default::default(),
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::default()),
    );
    let restored = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    assert!(restored.can_serve());
    let snapshot = store.snapshot.lock().unwrap().clone();
    let diagnostics =
        PluginPreparation::runtime_diagnostics(&runtime, &snapshot, Some(1), Some(&restored))
            .await
            .unwrap();
    let failure = diagnostics["instance-one"].failure.as_ref().unwrap();
    assert!(
        failure
            .message
            .contains("插件要求宿主 >=1.0.0, <2.0.0，当前为 3.0.0")
    );
    assert!(
        std::fs::read_dir(cache.path()).unwrap().next().is_none(),
        "incompatible packages never start"
    );
}

#[tokio::test]
async fn an_existing_failed_plugin_does_not_prevent_editing_an_unrelated_instance() {
    let (cache, store, runtime) = super::setup().await;
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
    super::wait_until_empty(cache.path()).await;
}
