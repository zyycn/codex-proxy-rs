//! 验证维护任务跨发布保活、通知合并及实例替换后的取消顺序

use gateway_admin::{
    model::{Revision, plugins::instances::PluginInstanceSnapshot},
    ports::plugins::PluginPreparation,
};
use gateway_core::{
    lifecycle::CancellationToken,
    routing::{ConfigRevision, RuntimeSnapshot},
    runtime::RuntimeSnapshotHandle,
    task::{WorkerContribution, WorkerDaemonContext, WorkerRunnable},
};
use gateway_plugin_runtime::PluginRuntime;
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

async fn publish(
    runtime: &PluginRuntime,
    snapshots: &RuntimeSnapshotHandle,
    instances: PluginInstanceSnapshot,
) {
    let revision = ConfigRevision::new(instances.config_revision.get()).unwrap();
    let prepared = PluginPreparation::prepare(runtime, instances)
        .await
        .unwrap();
    snapshots.publish(
        RuntimeSnapshot::new(
            revision,
            gateway_core::settings::SettingsValues::new(1, "smart", Default::default(), None, None),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_extensions(Some(prepared)),
    );
}

fn records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn wait_records(path: &Path, count: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let entries = records(path);
            if entries.len() >= count {
                return entries;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| panic!("missing maintenance records: {error}; {:?}", records(path)))
}

#[tokio::test]
async fn unrelated_publications_preserve_inflight_maintenance_and_replacement_waits_for_cancel() {
    let (cache, store, runtime) = super::super::setup_with_contributions(Contributions::from([
        crate::support::contribution(
            Capability::Maintenance,
            vec![Stage::Maintenance],
            vec![],
            vec![],
        ),
    ]))
    .await;
    let runtime = Arc::new(runtime);
    let markers = tempfile::tempdir().unwrap();
    let mut instances = store.snapshot.lock().unwrap().clone();
    let template = instances.instances[0].clone();
    instances.instances = ["a", "b"]
        .into_iter()
        .map(|id| {
            let mut instance = template.clone();
            instance.id = id.into();
            instance.configuration = json!({
                "maintenance_marker":markers.path().join(id),
                "execution_cancelled_marker":markers.path().join(id),
                "maintenance_wait_marker":markers.path().join(format!("release-{id}")),
                "call_context_marker":markers.path().join(format!("context-{id}")),
                "call_budgets_ms":{"maintenance":600_000},
            });
            instance
        })
        .collect();
    let snapshots = RuntimeSnapshotHandle::default();
    publish(&runtime, &snapshots, instances.clone()).await;
    let WorkerContribution::Registration(registration) =
        runtime.maintenance_worker(snapshots.clone()).unwrap()
    else {
        panic!("maintenance worker registration")
    };
    let WorkerRunnable::Daemon { task, .. } = registration.runnable else {
        panic!("maintenance daemon")
    };
    let stop = CancellationToken::new();
    let running = tokio::spawn({
        let stop = stop.clone();
        async move { task.run(WorkerDaemonContext::new(stop)).await.unwrap() }
    });
    let a = markers.path().join("a");
    let b = markers.path().join("b");
    assert_eq!(wait_records(&a, 1).await[0]["phase"], "start");
    assert_eq!(wait_records(&b, 1).await[0]["phase"], "start");
    let contexts = records(&markers.path().join("context-a"));
    let maintenance = contexts
        .iter()
        .find(|entry| entry["method"] == "plugin.reconcile")
        .expect("maintenance call context");
    assert_eq!(maintenance["context"]["timeout_ms"], 600_000);

    instances.config_revision = Revision::new(2).unwrap();
    instances.instances[1].revision = instances.config_revision;
    publish(&runtime, &snapshots, instances.clone()).await;
    let changed = wait_records(&b, 3).await;
    assert!(
        changed[1].get("call_id").is_some(),
        "旧维护必须先完成取消确认"
    );
    assert_eq!(changed[2]["phase"], "start");
    assert_eq!(records(&a).len(), 1, "无关实例发布不得重启 A 的在途维护");

    instances.config_revision = Revision::new(3).unwrap();
    instances.instances[1].revision = instances.config_revision;
    instances.instances[1].enabled = false;
    publish(&runtime, &snapshots, instances.clone()).await;
    assert!(wait_records(&b, 4).await[3].get("call_id").is_some());
    instances.config_revision = Revision::new(4).unwrap();
    instances.instances[1].revision = instances.config_revision;
    instances.instances[1].enabled = true;
    publish(&runtime, &snapshots, instances.clone()).await;
    assert_eq!(wait_records(&b, 5).await[4]["phase"], "start");
    assert_eq!(records(&a).len(), 1, "停用与恢复 B 不得取消 A");

    // 多次发布通知合并为一次后续对账，首轮仍使用原调用 ID 完成
    std::fs::write(markers.path().join("release-a"), []).unwrap();
    let completed = wait_records(&a, 4).await;
    assert_eq!(completed[1]["phase"], "done");
    assert_eq!(completed[1]["id"], completed[0]["id"]);
    assert_eq!(completed[2]["phase"], "start");
    assert_eq!(completed[3]["phase"], "done");
    assert_ne!(completed[2]["id"], completed[0]["id"]);
    assert_eq!(completed.len(), 4);

    stop.cancel();
    running.await.unwrap();
    assert!(wait_records(&b, 6).await[5].get("call_id").is_some());
    snapshots.suspend();
    runtime.shutdown().await;
    super::super::wait_until_empty(cache.path()).await;
}
