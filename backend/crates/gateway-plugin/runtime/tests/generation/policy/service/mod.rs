//! 验证插件服务中间件的组合、错误恢复、事务与取消边界

mod contract;
use crate::support::environment::{Environment, mutation};
use gateway_admin::model::{AdminErrorKind, settings::ReplaceRuntimeSettings};
use serde_json::json;

async fn usage_reader_lifecycle(ending: &str) {
    use gateway_admin::ports::plugins::PluginStateLifecycle;
    use std::time::Duration;

    let Some(environment) = Environment::create().await else {
        return;
    };
    let marker = environment.directory.path().join("usage-lifecycle");
    let (runtime, core) = environment
        .plugin(json!({"service":true,"mode":"usage_hold","records":marker}))
        .await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let services = admin.services();
    runtime.bind_services(&services.public_services()).unwrap();
    runtime.bind_usage_queries(&services.usage_data()).unwrap();
    let registry = services.public_services();
    let task = tokio::spawn(async move {
        registry
            .call("settings.admin_api_key_exists", json!(null))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.with_extension("opened").exists() {
            assert!(!task.is_finished(), "plugin must reach the open reader");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(environment.active_usage_transactions().await, 1);
    match ending {
        "cancel" => {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        "disable" => {
            let store = environment.store.admin_ports().plugins();
            let snapshot = store.load_instances().await.unwrap();
            services
                .plugins()
                .disable_instance(&snapshot.instances[0].id, &mutation())
                .await
                .unwrap();
            assert!(!task.is_finished());
            assert_eq!(environment.active_usage_transactions().await, 1);
            // 停用阻止新调用进入插件，旧调用仍沿冻结计划完成
            assert_eq!(
                services
                    .public_services()
                    .call("settings.admin_api_key_exists", json!(null))
                    .await
                    .unwrap(),
                json!(false)
            );
            std::fs::write(marker.with_extension("release"), b"release").unwrap();
            assert_eq!(task.await.unwrap().unwrap(), json!(true));
        }
        "quiesce" => {
            let snapshot = environment
                .store
                .admin_ports()
                .plugins()
                .load_instances()
                .await
                .unwrap();
            let instance = &snapshot.instances[0];
            runtime
                .quiesce_instance(&instance.id, &instance.artifact_sha256, instance.revision)
                .await;
            assert!(task.await.unwrap().is_err());
        }
        _ => unreachable!(),
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while environment.active_usage_transactions().await != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("usage transaction must be returned before runtime shutdown");
    assert_eq!(
        services
            .usage_data()
            .get_record(gateway_admin::model::usage_data::GetUsageRecord {
                id: "held-usage".into(),
            })
            .await
            .unwrap()
            .id,
        "held-usage"
    );
    drop(services);
    drop(admin);
    drop(core);
    runtime.shutdown().await;
    drop(runtime);
    environment.close().await;
}
