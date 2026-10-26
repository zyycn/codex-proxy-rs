//! 实例运行诊断与包检查缓存的查询回归测试

use super::*;
use gateway_admin::model::plugins::instances::PluginInstanceRuntimeLoad;

#[tokio::test]
async fn instance_list_uses_runtime_diagnostics_and_keeps_running_compatibility_derived() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let expected = PluginInstanceRuntime {
        status: PluginInstanceRuntimeStatus::PreparationFailed,
        actual_revision: Some(9),
        actual_artifact_sha256: Some(OLD_ARTIFACT.into()),
        failure: Some(PluginInstanceRuntimeFailure {
            code: "unavailable".into(),
            message: "插件候选准备失败".into(),
        }),
        draining_revisions: vec![8],
        load: Some(PluginInstanceRuntimeLoad {
            active_calls: 3,
            executing_calls: 1,
            queued_calls: 1,
            resuming_calls: 1,
            pending_callbacks: 2,
            active_streams: 0,
        }),
    };
    fixture.data.lock().unwrap().diagnostics = Some(BTreeMap::from([(
        "00000000-0000-7000-8000-000000000001".into(),
        expected.clone(),
    )]));

    let views = service(fixture, Arc::new(Published::default()))
        .instances()
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    assert!(!views[0].running);
    assert_eq!(views[0].runtime, expected);
}

#[tokio::test]
async fn instance_list_falls_back_without_inventing_runtime_failures() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let views = service(fixture, Arc::new(Published::default()))
        .instances()
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].runtime.status,
        PluginInstanceRuntimeStatus::Unknown
    );
    assert!(!views[0].running);
    assert!(views[0].runtime.failure.is_none());
    assert!(views[0].runtime.actual_revision.is_none());
    assert!(views[0].runtime.load.is_none());
}

#[tokio::test]
async fn disabled_instance_without_runtime_diagnostics_has_a_known_disabled_status() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    fixture.data.lock().unwrap().snapshot.instances[0].enabled = false;
    let views = service(fixture, Arc::new(Published::default()))
        .instances()
        .await
        .unwrap();
    assert_eq!(
        views[0].runtime.status,
        PluginInstanceRuntimeStatus::Disabled
    );
    assert!(!views[0].running);
    assert!(views[0].runtime.failure.is_none());
    assert!(views[0].runtime.load.is_none());
}

#[tokio::test]
async fn matching_publication_without_process_diagnostics_does_not_imply_running() {
    for diagnostics in [None, Some(BTreeMap::new())] {
        let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
        fixture.data.lock().unwrap().diagnostics = diagnostics;
        let revision = fixture.snapshot().config_revision.get();
        let published = RuntimeSnapshotHandle::new(
            gateway_core::routing::RuntimeSnapshot::new(
                ConfigRevision::new(revision).unwrap(),
                gateway_core::settings::SettingsValues::new(
                    1,
                    "round_robin",
                    BTreeMap::new(),
                    None,
                    None,
                ),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        );
        let service = PluginsService::new(
            fixture.clone(),
            fixture.clone(),
            PluginDistributionPorts::new(Arc::new(TestPluginPorts), Arc::new(TestPluginPorts)),
            Arc::new(Published::default()),
            fixture.clone(),
            published,
            fixture,
        );
        let view = service.instances().await.unwrap().remove(0);
        assert_eq!(view.published_revision, Some(revision));
        assert_eq!(view.runtime.status, PluginInstanceRuntimeStatus::Unknown);
        assert!(!view.running);
        assert!(view.runtime.actual_revision.is_none());
        assert!(view.runtime.actual_artifact_sha256.is_none());
        assert!(view.runtime.failure.is_none());
    }
}

#[tokio::test]
async fn cached_package_query_is_not_blocked_by_another_package_inspection() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let service = Arc::new(service(fixture.clone(), Arc::new(Published::default())));
    service.instances().await.unwrap();
    let gate = Arc::new(InspectionGate {
        digest: NEW_ARTIFACT.into(),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *fixture.inspection_gate.lock().unwrap() = Some(gate.clone());
    fixture.data.lock().unwrap().snapshot.instances[0].artifact_sha256 = NEW_ARTIFACT.into();
    let inspecting = {
        let service = service.clone();
        tokio::spawn(async move { service.instances().await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), gate.started.notified())
        .await
        .expect("another package inspection started");

    fixture.data.lock().unwrap().snapshot.instances[0].artifact_sha256 = OLD_ARTIFACT.into();
    let cached =
        tokio::time::timeout(std::time::Duration::from_millis(100), service.instances()).await;
    gate.release.notify_one();
    inspecting.await.unwrap().unwrap();
    assert_eq!(
        cached
            .expect("cached query must not wait for inspection")
            .unwrap()[0]
            .instance
            .artifact_sha256,
        OLD_ARTIFACT
    );
    assert_eq!(fixture.data.lock().unwrap().archive_reads, 2);
}
