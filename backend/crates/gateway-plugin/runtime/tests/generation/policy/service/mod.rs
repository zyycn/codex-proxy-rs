//! 验证插件服务中间件的组合、错误恢复、事务与取消边界

use crate::support::environment::{Environment, mutation};
use gateway_admin::model::{AdminErrorKind, settings::ReplaceRuntimeSettings};
use serde_json::json;

#[tokio::test]
async fn plugin_error_details_survive_public_service_composition() {
    let Some(environment) = Environment::create().await else {
        return;
    };
    let (runtime, core) = environment
        .plugin(json!({"service":true,"mode":"fault"}))
        .await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let services = admin.services();
    runtime.bind_services(&services.public_services()).unwrap();
    let error = services
        .public_services()
        .call("settings.load", json!(null))
        .await
        .unwrap_err();
    assert_eq!(error.message, "service plugin fixture error");
    let details = error.details.unwrap();
    assert_eq!(details["code"], "capacity");
    assert_eq!(
        details["details"],
        json!({"opaque":"unfiltered","empty":"","zero":0,"disabled":false,"missing":null})
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn explicit_service_dispatch_composes_once_and_preserves_native_transactions() {
    let Some(environment) = Environment::create().await else {
        return;
    };
    let records = environment.directory.path().join("services.jsonl");
    let (runtime, core) = environment
        .plugin(json!({"service":true,"mode":"rewrite","records":records}))
        .await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let services = admin.services();
    runtime.bind_services(&services.public_services()).unwrap();

    let original = environment
        .store
        .admin_ports()
        .settings()
        .load_runtime_settings()
        .await
        .unwrap();
    let visible = services.settings().load().await.unwrap();
    assert_eq!(visible.request_interval_ms, original.request_interval_ms);
    assert_eq!(visible.config_revision, original.config_revision);
    let through_registry = services
        .public_services()
        .call("settings.load", json!(null))
        .await
        .unwrap();
    let typed: gateway_plugin_sdk::call::services::settings::RuntimeSettings =
        serde_json::from_value(through_registry).unwrap();
    assert_eq!(typed.request_interval_ms, 4321);
    // 返回层改写不是持久化写入；原 owner 保留真实配置事实
    assert_eq!(
        environment
            .store
            .admin_ports()
            .settings()
            .load_runtime_settings()
            .await
            .unwrap()
            .request_interval_ms,
        original.request_interval_ms
    );

    let mut command = ReplaceRuntimeSettings::from(original);
    command.request_interval_ms = 999;
    let saved = services
        .public_services()
        .call(
            "settings.replace",
            serde_json::to_value((mutation(), command.clone())).unwrap(),
        )
        .await
        .unwrap();
    let saved: gateway_admin::model::settings::RuntimeSettings =
        serde_json::from_value(saved).unwrap();
    assert_eq!(saved.request_interval_ms, 1234);
    assert!(saved.config_revision > command.expected_revision);
    let current = environment
        .store
        .admin_ports()
        .settings()
        .load_runtime_settings()
        .await
        .unwrap();
    assert_eq!(current.request_interval_ms, 1234);
    assert_eq!(
        core.snapshots()
            .snapshot_for_diagnostics()
            .unwrap()
            .revision()
            .get(),
        saved.config_revision.get()
    );
    let error = services
        .settings()
        .replace(&mutation(), command.clone())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    let error = services
        .public_services()
        .call(
            "settings.replace",
            serde_json::to_value((mutation(), command)).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, "conflict");
    assert!(!error.message.is_empty());

    // 组合位于主动调用入口；内部 pricing 不重复进入插件链
    let error = services
        .public_services()
        .call(
            "settings.update_pricing",
            serde_json::to_value((
                mutation(),
                gateway_admin::model::pricing::UpdatePricing {
                    provider: "missing-provider".into(),
                    models: vec!["model".into()],
                    change: gateway_admin::model::pricing::PricingChange::Reset,
                },
            ))
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, "invalid");
    let records: Vec<serde_json::Value> = std::fs::read_to_string(&records)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record["operation"] == "settings.update_pricing")
            .count(),
        1
    );
    assert!(
        !records
            .iter()
            .any(|record| record["operation"] == "settings.pricing")
    );

    drop(services);
    drop(admin);
    drop(core);
    runtime.shutdown().await;
    environment.close().await;
}

#[tokio::test]
async fn service_middleware_can_recover_a_native_error_without_changing_the_default_service() {
    let Some(environment) = Environment::create().await else {
        return;
    };
    let (runtime, core) = environment
        .plugin(json!({"service":true,"mode":"recover"}))
        .await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let services = admin.services();
    runtime.bind_services(&services.public_services()).unwrap();
    assert!(
        services
            .settings()
            .preview_client_profile("missing-provider", None)
            .await
            .is_err()
    );
    let recovered = services
        .public_services()
        .call(
            "settings.preview_client_profile",
            json!(["missing-provider", null]),
        )
        .await
        .unwrap();
    assert_eq!(recovered, json!({}));
    assert_eq!(
        services
            .public_services()
            .call("settings.admin_api_key_exists", json!(null))
            .await
            .unwrap(),
        true
    );
    assert!(!services.settings().admin_api_key_exists().await.unwrap());
    assert!(
        !environment
            .store
            .admin_ports()
            .settings()
            .admin_api_key_exists()
            .await
            .unwrap()
    );
    assert!(
        services
            .public_services()
            .call("settings.unknown", json!(null))
            .await
            .is_err()
    );
    drop(services);
    drop(admin);
    drop(core);
    runtime.shutdown().await;
    environment.close().await;
}

#[tokio::test]
async fn cancelling_explicit_service_call_releases_plugin_handler_before_shutdown() {
    let Some(environment) = Environment::create().await else {
        return;
    };
    let records = environment.directory.path().join("blocked.jsonl");
    let (runtime, core) = environment
        .plugin(json!({"service":true,"mode":"block","records":records}))
        .await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let services = admin.services();
    runtime.bind_services(&services.public_services()).unwrap();
    let registry = services.public_services();
    let task = tokio::spawn(async move { registry.call("settings.load", json!(null)).await });
    let wait_for = |path: std::path::PathBuf| async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("plugin lifecycle signal");
    };
    wait_for(records.clone()).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_for(records.with_extension("closed")).await;
    drop(services);
    drop(admin);
    drop(core);
    runtime.shutdown().await;
    environment.close().await;
}

#[tokio::test]
async fn cli_service_callback_uses_the_same_public_settings_and_plugin_chain() {
    use std::sync::Arc;
    struct Pricing;
    #[async_trait::async_trait]
    impl gateway_admin::ports::pricing::PricingSource for Pricing {
        async fn fetch(
            &self,
        ) -> Result<
            gateway_admin::model::pricing::PricingSyncPreview,
            gateway_admin::model::AdminError,
        > {
            unreachable!("reading settings does not fetch external pricing")
        }
    }
    let Some(environment) = Environment::create_command().await else {
        return;
    };
    environment.install_plugin(json!({
        "plugin_id":"test.command-source",
        "command_registration":{"commands":[{"name":"settings","description":"Read public settings"}]},
        "command_service_request":{"operation":"settings.load","input":null},
    })).await;
    environment
        .install_plugin(json!({"service":true,"mode":"rewrite"}))
        .await;
    let (runtime, core) = environment.command_plane().await;
    let settings = gateway_admin::initialize_settings(
        environment.store.admin_ports().settings(),
        core.snapshot_control(),
        crate::support::native::admin_registry(),
        Arc::new(Pricing),
    );
    let mut registry = gateway_admin::service::Registry::new({
        let snapshots = core.snapshots();
        let middleware = runtime.middleware_registry();
        Arc::new(move || middleware.resolve(snapshots.snapshot_for_diagnostics()?.extensions()?))
    });
    registry.register_settings(&settings).unwrap();
    let registry = Arc::new(registry);
    runtime.bind_services(&registry).unwrap();
    let instances = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    let command = instances
        .instances
        .iter()
        .find(|instance| instance.configuration.get("command_registration").is_some())
        .unwrap();
    let commands = runtime.prepare_command_line().await.unwrap();
    let output = commands
        .execute(&command.id, "settings", &[])
        .await
        .unwrap();
    let result: gateway_plugin_sdk::call::services::Response =
        serde_json::from_str(&output.stdout).unwrap();
    let returned: gateway_plugin_sdk::call::services::settings::RuntimeSettings =
        serde_json::from_value(result.unwrap()).unwrap();
    assert_eq!(returned.request_interval_ms, 4321);
    assert_ne!(
        environment
            .store
            .admin_ports()
            .settings()
            .load_runtime_settings()
            .await
            .unwrap()
            .request_interval_ms,
        4321
    );
    commands.shutdown().await;
    drop(core);
    runtime.shutdown().await;
    environment.close().await;
}

#[tokio::test]
async fn service_chain_preserves_onion_order_parent_scope_and_native_results_without_a_database() {
    use super::*;
    use gateway_core::middleware::{compose, service as core};

    let directory = tempfile::tempdir().unwrap();
    let records = directory.path().join("onion.jsonl");
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Service],
            vec!["service".into()],
            vec!["service".into()],
        )]),
    );
    let (cache, runtime) = setup_package(
        [("inner", 20), ("ancestor", 0), ("outer", 10)]
            .into_iter()
            .map(|(id, order)| InstanceFixture {
                id,
                configuration: json!({"service":true,"mode":"onion","records":records}),
                bindings: vec![binding(
                    MIDDLEWARE_CONTRIBUTION,
                    "service",
                    order,
                    PluginFailurePolicy::Reject,
                )],
            })
            .collect(),
        package,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let input = json!({"opaque":[null,false,0,""]});
    for reject in [false, true] {
        let observed = calls.clone();
        let expected_input = input.clone();
        let expected = if reject {
            Err(core::Error {
                kind: "fixture".into(),
                message: "native error".into(),
                details: Some(input.clone()),
            })
        } else {
            Ok(input.clone())
        };
        let result = expected.clone();
        let actual = plan
            .handle_service(
                core::Context {
                    operation: "fixture.echo",
                    request_id: "request".into(),
                    call_id: "child".into(),
                    parent_call_id: Some("parent".into()),
                    cancellation: CancellationToken::new(),
                    extensions: ExtensionCallScope::default()
                        .extending("ancestor".into())
                        .unwrap(),
                    plan: plan.clone(),
                },
                input.clone(),
                compose(Vec::new(), move |input| {
                    Box::pin(async move {
                        observed.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(input, expected_input);
                        result
                    })
                }),
            )
            .await;
        assert_eq!(actual, expected);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events = std::fs::read_to_string(records)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 8);
    for round in events.chunks_exact(4) {
        let phases = round
            .iter()
            .map(|event| {
                (
                    event["instance"].as_str().unwrap(),
                    event["phase"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            [
                ("outer", "enter"),
                ("inner", "enter"),
                ("inner", "exit"),
                ("outer", "exit")
            ]
        );
        for event in round {
            assert_eq!(event["operation"], "fixture.echo");
            assert_eq!(event["request_id"], "request");
            assert_eq!(event["call_id"], "child");
            assert_eq!(event["parent_call_id"], "parent");
        }
    }
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
    drop(cache);
}
