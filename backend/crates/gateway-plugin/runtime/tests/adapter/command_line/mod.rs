//! 验证插件命令帮助、调用授权、输出与失败取消后的进程回收

mod execution;
mod parameters;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use gateway_admin::{
    model::{
        Revision,
        plugins::instances::{PluginInstance, PluginInstanceSnapshot},
    },
    ports::plugins::PluginPackageInspector,
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRuntime, PluginRuntimeConfig, RpcLimits,
};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use serde_json::{Value, json};

use crate::support::store::Store;

async fn setup(configuration: Value) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    setup_with_limits(configuration, RpcLimits::default()).await
}

async fn setup_with_limits(
    configuration: Value,
    rpc_limits: RpcLimits,
) -> (tempfile::TempDir, Arc<Store>, PluginRuntime) {
    let directory = tempfile::tempdir().unwrap();
    let archive = crate::support::package_with_contributions(
        crate::support::worker(),
        Contributions::from([crate::support::contribution(
            Capability::CommandLine,
            vec![Stage::CommandLine],
            vec![],
            vec![],
        )]),
    );
    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(archive, None)
        .await
        .unwrap();
    let instance = PluginInstance {
        id: "commands".into(),
        name: "Commands".into(),
        artifact_sha256: artifact.metadata.sha256.clone(),
        enabled: true,
        trusted_process: true,
        configuration,
        secrets: BTreeMap::new(),

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
            cache_directory: directory.path().join("cache"),
            host_version: "1.0.0".parse().unwrap(),
            package_limits: PackageLimits::default(),
            rpc_limits,
            restart_circuit: Default::default(),
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::default()),
    );
    (directory, store, runtime)
}

fn configuration(parameters: Value) -> Value {
    json!({"command_registration":{"commands":[{"name":"inspect","description":"检查参数","parameters":parameters}]},"command_echo":true})
}

#[tokio::test]
async fn command_help_does_not_execute_and_stale_authorization_cannot_execute() {
    let (directory, store, runtime) = setup(configuration(json!([]))).await;
    let marker = directory.path().join("commands.jsonl");
    store.snapshot.lock().unwrap().instances[0].configuration["command_marker"] = json!(marker);
    let commands = runtime.prepare_command_line().await.unwrap();
    assert!(
        commands
            .help(None, None)
            .unwrap()
            .contains("commands inspect")
    );
    assert!(
        commands
            .help(Some("commands"), Some("inspect"))
            .unwrap()
            .contains("--help")
    );
    assert!(!marker.exists());
    assert!(commands.execute("commands", "missing", &[]).await.is_err());
    assert!(!marker.exists());
    let output = commands.execute("commands", "inspect", &[]).await.unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    store.snapshot.lock().unwrap().instances[0].enabled = false;
    let error = commands
        .execute("commands", "inspect", &[])
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error,
        gateway_plugin_runtime::PluginCommandError::Invalid(_)
    ));
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn command_returns_separate_output_and_exit_code_without_saving_on_failure() {
    let mut config = configuration(json!([]));
    config["command_echo"] = json!(false);
    config["command_result"] = json!({"stdout":"out\n","stderr":"err\n","exit_code":17});
    let (_directory, _store, runtime) = setup(config).await;
    let commands = runtime.prepare_command_line().await.unwrap();
    let output = commands.execute("commands", "inspect", &[]).await.unwrap();
    assert_eq!(
        (
            output.stdout.as_str(),
            output.stderr.as_str(),
            output.exit_code
        ),
        ("out\n", "err\n", 17)
    );
    assert_eq!(output.saved_accounts, 0);
}

#[tokio::test]
async fn failed_or_cancelled_commands_are_not_replayed_and_children_are_reaped() {
    use gateway_core::lifecycle::CancellationToken;
    use gateway_plugin_runtime::PluginCommandError;
    use std::time::Duration;

    for failure in ["timeout", "crash", "cancel"] {
        let mut config = configuration(json!([]));
        config["command_wait"] = json!(true);
        config["command_crash"] = json!(failure == "crash");
        let limits = RpcLimits::default();
        let (directory, store, runtime) = setup_with_limits(config, limits).await;
        let marker = directory.path().join("commands.jsonl");
        store.snapshot.lock().unwrap().instances[0].configuration["command_marker"] = json!(marker);
        let commands = runtime.prepare_command_line().await.unwrap();
        let cancellation = CancellationToken::new();
        let invoke = commands.execute_cancellable("commands", "inspect", &[], &cancellation);
        let interrupt = async {
            if failure != "crash" {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !marker.exists() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                if failure == "cancel" {
                    cancellation.cancel();
                } else {
                    // 正常完成注册并确认命令已进入子进程后，只推进执行期限
                    tokio::time::pause();
                    tokio::time::advance(limits.maximum_call_timeout + Duration::from_millis(1))
                        .await;
                }
            }
        };
        let (result, ()) = tokio::join!(invoke, interrupt);
        if failure == "timeout" {
            tokio::time::resume();
        }
        let error = result.err().unwrap();
        assert!(matches!(
            error,
            PluginCommandError::Incomplete {
                saved_accounts: 0,
                ..
            }
        ));
        if failure == "cancel" {
            assert!(error.to_string().contains("调用已取消"));
        } else if failure == "timeout" {
            assert!(error.to_string().contains("超时") || error.to_string().contains("总期限"));
        } else {
            assert!(error.to_string().contains("插件进程不可用"));
        }
        commands.shutdown().await;
        let invocations = std::fs::read_to_string(marker).unwrap();
        assert_eq!(invocations.lines().count(), 1, "{failure}");
        #[cfg(target_os = "linux")]
        {
            let invocation: Value = serde_json::from_str(invocations.trim()).unwrap();
            let pid = invocation["pid"].as_u64().unwrap();
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
    }
}
