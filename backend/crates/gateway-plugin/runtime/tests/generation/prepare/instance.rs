//! 验证实例握手、声明与重启熔断的生命周期

use super::wait_until_unready;
use gateway_admin::model::Revision;
use gateway_admin::model::plugins::instances::PluginInstanceRuntimeStatus;
use gateway_admin::ports::plugins::PluginPreparation;
use gateway_core::routing::ConfigRevision;
use gateway_core::routing::extensions::ExtensionPreparationPort;
use gateway_plugin_sdk::Capability;
use gateway_plugin_sdk::Contributions;
use gateway_plugin_sdk::Stage;
use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

fn startup_count(marker: &Path) -> usize {
    std::fs::read_to_string(marker)
        .map(|content| content.lines().count())
        .unwrap_or_default()
}

#[tokio::test]
async fn cold_preparation_overlaps_startup_with_bounded_concurrency() {
    let (cache, store, runtime) = super::super::setup().await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    let template = snapshot.instances[0].clone();
    let gate = cache.path().join("startup-gate");
    let markers: Vec<_> = (0..3)
        .map(|index| cache.path().join(format!("startup-{index}")))
        .collect();
    snapshot.instances = markers
        .iter()
        .enumerate()
        .map(|(index, marker)| {
            let mut instance = template.clone();
            instance.id = format!("instance-{index}");
            instance.configuration = serde_json::json!({
                "startup_marker": marker,
                "startup_gate": gate,
            });
            instance
        })
        .collect();
    let release = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while startup_count(&markers[0]) == 0 || startup_count(&markers[1]) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both startup handshakes must progress before either is ready");
        assert_eq!(
            startup_count(&markers[2]),
            0,
            "preparation keeps its concurrency budget"
        );
        std::fs::write(&gate, []).unwrap();
    };
    let (prepared, ()) = tokio::join!(PluginPreparation::prepare(&runtime, snapshot), release);
    let prepared = prepared.unwrap();
    assert!(markers.iter().all(|marker| startup_count(marker) == 1));
    drop(prepared);
    runtime.shutdown().await;
}

#[tokio::test]
async fn registration_with_a_changed_contribution_id_is_rejected() {
    let (cache, store, runtime) = super::super::setup_with_contributions(Contributions::from([
        crate::support::contribution(
            Capability::Observer,
            vec![Stage::Observation],
            Vec::new(),
            Vec::new(),
        ),
    ]))
    .await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    let candidate = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("the declared contribution prepares before its registration id changes");
    drop(candidate);
    super::super::wait_until_empty(cache.path()).await;

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
async fn restart_circuit_caps_short_lived_crashes_and_a_new_revision_recovers() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_secs(1),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
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

    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_resets_after_a_stable_incarnation() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_millis(80),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("stable-reset-starts.jsonl");
    let exit = controls.path().join("stable-exit");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker":marker,
        "startup_failures":[true,true,false,true,true],
        "exit_after_ready_signals":[null,null,exit],
    });
    // 握手前失败的有效运行时间固定为零；不能用短 sleep 假定宿主一定及时观察到退出
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
    // 明确等到稳定窗口之后才允许第三个进程退出；调度变慢不会把短命进程误变为稳定进程
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_ignores_planned_generation_shutdown() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::MIN,
        stability_window: Duration::from_secs(60),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("planned-shutdown-starts.jsonl");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({"startup_marker":marker});
    let first = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .expect("first generation");
    let instance = &snapshot.instances[0];
    gateway_admin::ports::plugins::PluginStateLifecycle::quiesce_instance(
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_counts_exit_before_the_candidate_is_indexed() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_secs(1),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
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

    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
        &runtime, &snapshot, None, None,
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn restart_circuit_counts_a_started_process_that_exits_during_handshake() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(2).unwrap(),
        stability_window: Duration::from_millis(10),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn reused_session_failure_counts_once_across_multiple_generations() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::new(3).unwrap(),
        stability_window: Duration::from_millis(50),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
    let controls = tempfile::tempdir().unwrap();
    let marker = controls.path().join("shared-generations.jsonl");
    let exit = controls.path().join("shared-exit");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "startup_marker": marker,
        "startup_failures": [false, true, true, true],
        "exit_after_ready_signals": [exit],
    });
    let mut other = snapshot.instances[0].clone();
    other.id = "other-instance".into();
    other.configuration = serde_json::json!({});
    snapshot.instances.push(other);
    let published = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();
    let mut retained = Vec::new();
    for revision in 2..=4 {
        snapshot.config_revision = Revision::new(revision).unwrap();
        snapshot.instances[1].revision = snapshot.config_revision;
        retained.push(
            PluginPreparation::prepare(&runtime, snapshot.clone())
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        startup_count(&marker),
        1,
        "unrelated edits share the healthy process"
    );
    tokio::time::sleep(restart_circuit.stability_window).await;
    std::fs::write(&exit, b"exit").unwrap();
    wait_until_unready(&published).await;
    assert!(retained.iter().all(|set| !set.is_ready()));
    // 同一进程故障只扣一次预算；两个新的握手失败后才熔断
    for revision in 5..=6 {
        snapshot.config_revision = Revision::new(revision).unwrap();
        snapshot.instances[1].revision = snapshot.config_revision;
        let candidate = PluginPreparation::prepare(&runtime, snapshot.clone())
            .await
            .unwrap();
        assert_eq!(startup_count(&marker), (revision - 3) as usize);
        drop(candidate);
    }
    snapshot.config_revision = Revision::new(7).unwrap();
    snapshot.instances[1].revision = snapshot.config_revision;
    let recovered = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();
    assert_eq!(
        startup_count(&marker),
        3,
        "the open circuit prevents another start"
    );
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(7),
        Some(&recovered),
    )
    .await
    .unwrap();
    assert_eq!(
        diagnostics["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("unavailable")
    );
    drop(retained);
    drop(recovered);
    drop(published);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn abandoned_candidate_keeps_shared_process_failure_in_restart_budget() {
    let restart_circuit = gateway_plugin_runtime::PluginRestartCircuitConfig {
        maximum_failures: NonZeroUsize::MIN,
        stability_window: Duration::from_secs(1),
    };
    let (cache, store, runtime) = super::super::setup_with_restart_circuit(restart_circuit).await;
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
    assert_eq!(startup_count(&marker), 1);
    drop(candidate);
    assert!(published.is_ready());
    std::fs::write(&old_exit, b"exit").unwrap();
    wait_until_unready(&published).await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let diagnostics =
                gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
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
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(3),
        Some(&recovered),
    )
    .await
    .unwrap();
    assert_eq!(
        diagnostics["instance-one"]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("unavailable"),
    );
    assert_eq!(startup_count(&marker), 1);

    drop(recovered);
    drop(published);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn a_host_version_mismatch_still_restores_the_enabled_plugin() {
    use std::sync::Arc;
    let (cache, store, _) = super::super::setup().await;
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
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(1),
        Some(&restored),
    )
    .await
    .unwrap();
    assert!(restored.is_ready());
    assert!(snapshot.instances[0].enabled);
    assert_eq!(
        diagnostics["instance-one"].status,
        PluginInstanceRuntimeStatus::Running
    );
    assert!(diagnostics["instance-one"].failure.is_none());
    drop(restored);
    runtime.shutdown().await;
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn declared_registration_budget_is_not_cut_short_by_process_preparation_timeout() {
    let (cache, store, runtime) = super::super::setup().await;
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "registration_delay_ms":31_000,
        "call_budgets_ms":{"registration":40_000},
    });
    let candidate = tokio::time::timeout(
        Duration::from_secs(45),
        PluginPreparation::prepare(&runtime, snapshot),
    )
    .await
    .expect("registration stays bounded by its declared budget")
    .expect("ready process may register beyond the startup control deadline");
    assert!(candidate.is_ready());
    drop(candidate);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn shutdown_cancels_long_registration_and_reclaims_the_unpublished_candidate() {
    let (cache, store, runtime) = super::super::setup().await;
    let runtime = std::sync::Arc::new(runtime);
    let markers = tempfile::tempdir().unwrap();
    let started = markers.path().join("registration");
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    snapshot.instances[0].configuration = serde_json::json!({
        "registration_started_marker":started,
        "registration_delay_ms":600_000,
        "call_budgets_ms":{"registration":600_000},
    });
    let preparing = tokio::spawn({
        let runtime = std::sync::Arc::clone(&runtime);
        async move { PluginPreparation::prepare(runtime.as_ref(), snapshot).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), runtime.shutdown())
        .await
        .expect(
            "shutdown must cancel candidate preparation instead of waiting for its long budget",
        );
    assert!(preparing.await.unwrap().is_err());
    super::super::wait_until_empty(cache.path()).await;
}
