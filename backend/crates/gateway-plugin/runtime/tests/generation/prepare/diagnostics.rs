//! 验证准备、发布进程故障与排空状态的诊断投影

use super::wait_until_unready;
use gateway_admin::model::Revision;
use gateway_admin::model::plugins::instances::{
    PluginInstanceRuntimeLoad, PluginInstanceRuntimeStatus,
};
use gateway_admin::ports::plugins::PluginPreparation;
use gateway_core::routing::ConfigRevision;
use gateway_core::routing::extensions::ExtensionPreparationPort;

#[tokio::test]
async fn diagnostics_distinguish_running_failed_preparation_and_draining_generations() {
    let (cache, store, runtime) = super::super::setup().await;
    let snapshot = store.snapshot.lock().unwrap().clone();
    let active = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();

    let running = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
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
    assert_eq!(
        running.load,
        Some(PluginInstanceRuntimeLoad {
            active_calls: 0,
            executing_calls: 0,
            queued_calls: 0,
            resuming_calls: 0,
            pending_callbacks: 0,
            active_streams: 0,
        })
    );

    let mut failed = snapshot.clone();
    failed.config_revision = Revision::new(2).unwrap();
    failed.instances[0].revision = Revision::new(2).unwrap();
    failed.instances[0].configuration = serde_json::json!({"startup":"fail"});
    assert!(
        PluginPreparation::prepare(&runtime, failed.clone())
            .await
            .is_err()
    );
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
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
    assert_eq!(failed_runtime.load, running.load);

    let mut disabled = failed;
    disabled.config_revision = Revision::new(3).unwrap();
    disabled.instances[0].revision = Revision::new(3).unwrap();
    disabled.instances[0].enabled = false;
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn preparing_diagnostics_never_wait_for_the_serialized_prepare_io() {
    let (cache, store, runtime) = super::super::setup().await;
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
        gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
            &runtime,
            &diagnostic_snapshot,
            None,
            None,
        ),
    )
    .await
    .expect("diagnostics must not wait for plugin startup")
    .expect("runtime diagnostics");
    assert_eq!(
        diagnostics["instance-one"].status,
        PluginInstanceRuntimeStatus::Preparing
    );
    assert!(diagnostics["instance-one"].load.is_none());

    let candidate = preparation.await.expect("delayed candidate");
    drop(candidate);
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn diagnostics_report_a_published_process_fault_without_plugin_details() {
    let (cache, store, runtime) = super::super::setup().await;
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

    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
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
    super::super::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn a_published_process_crash_preserves_the_serving_snapshot_and_other_plugin_status() {
    let (cache, store, runtime) = super::super::setup().await;
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
    let diagnostics = gateway_admin::ports::plugins::PluginRuntimeDiagnostics::runtime_diagnostics(
        &runtime,
        &snapshot,
        Some(1),
        Some(&generation),
    )
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
    super::super::wait_until_empty(cache.path()).await;
}
