mod authentication;
mod configuration;
mod data;
mod maintenance;
mod observer;
mod policy;
mod prepare;
mod private_state;

use gateway_admin::{
    model::{
        Revision,
        plugins::instances::{PluginInstance, PluginInstanceSnapshot},
    },
    ports::plugins::PluginPackageInspector,
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRestartCircuitConfig, PluginRuntime,
    PluginRuntimeConfig, RpcLimits,
};
use gateway_plugin_sdk::Contributions;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::support::store::Store;

async fn setup() -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    setup_with_contributions(Contributions::new()).await
}

async fn setup_with_contributions(
    contributes: Contributions,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    setup_with_contributions_and_restart_circuit(contributes, Default::default()).await
}

async fn setup_with_restart_circuit(
    restart_circuit: PluginRestartCircuitConfig,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    setup_with_contributions_and_restart_circuit(Contributions::new(), restart_circuit).await
}

async fn setup_with_contributions_and_restart_circuit(
    contributes: Contributions,
    restart_circuit: PluginRestartCircuitConfig,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    setup_with_permissions(contributes, restart_circuit, vec![]).await
}

async fn setup_with_permissions(
    contributes: Contributions,
    restart_circuit: PluginRestartCircuitConfig,
    permissions: Vec<gateway_plugin_sdk::Permission>,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    let cache = tempfile::tempdir().unwrap();
    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(
            crate::support::package_with_contributions(
                crate::support::worker(),
                permissions,
                contributes,
            ),
            None,
        )
        .await
        .unwrap();
    let instance = PluginInstance {
        id: "instance-one".into(),
        name: "Example".into(),
        artifact_sha256: artifact.metadata.sha256.clone(),
        enabled: true,
        trusted_process: true,
        configuration: serde_json::json!({}),
        secrets: BTreeMap::new(),
        grants: vec![],
        bindings: vec![],
        revision: Revision::new(1).unwrap(),
    };
    let store = Arc::new(Store {
        artifacts: BTreeMap::from([(artifact.metadata.sha256.clone(), artifact)]),
        snapshot: Mutex::new(PluginInstanceSnapshot {
            config_revision: Revision::new(1).unwrap(),
            instances: vec![instance],
        }),
    });
    let runtime = PluginRuntime::new(
        store.clone(),
        store.clone(),
        PluginRuntimeConfig {
            cache_directory: cache.path().to_owned(),
            host_version: "1.0.0".parse().unwrap(),
            package_limits: PackageLimits::default(),
            rpc_limits: RpcLimits::default(),
            restart_circuit,
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::new(
            std::num::NonZeroUsize::new(128).unwrap(),
        )),
    );
    (cache, store, runtime)
}

async fn wait_until_empty(cache: &std::path::Path) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if std::fs::read_dir(cache).unwrap().next().is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
