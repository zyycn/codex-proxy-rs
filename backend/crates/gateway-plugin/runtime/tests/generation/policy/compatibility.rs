//! 验证中间件 Fast 三态设置与不兼容插件的实例故障隔离

use super::*;
use gateway_core::{policy::ClientPolicy, routing::request_settings::RequestSettings};
use serde_json::{Value, json};

#[tokio::test]
async fn middleware_preserves_three_state_fast_and_sources() {
    for (mode, expected) in [
        (FastMode::Default, FastMode::Default),
        (FastMode::Enabled, FastMode::Enabled),
        (FastMode::Disabled, FastMode::Disabled),
        (FastMode::Enabled, FastMode::Disabled),
        (FastMode::Disabled, FastMode::Default),
    ] {
        let snapshot = gateway_core::routing::RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            gateway_core::settings::SettingsValues::new(1, "smart", Default::default(), None, None),
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let settings = RequestSettings::new(Arc::new(snapshot)).with_execution(
            &ClientPolicy::new(
                middleware_context(ClientTransport::HttpJson)
                    .client_key_id()
                    .clone(),
                gateway_core::policy::PlaintextClientApiKey::new("fixture-key").unwrap(),
                Arc::new(gateway_core::routing::FrozenAccountScope::new(
                    Arc::default(),
                    gateway_core::routing::ClientRoutingScope::all_accounts(),
                )),
                true,
                Default::default(),
            ),
            Some(60_000),
        );
        let mut seed = settings.execution_values().unwrap();
        seed.fast_mode = mode;
        let settings = settings.replace_execution(&seed, "seed").unwrap();
        let wire = serde_json::to_value(&seed).unwrap();
        let mut updated = wire.clone();
        updated["fast_mode"] = json!(expected.as_str());
        updated["timeout_ms"] = json!(90_000);
        let contribution = crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        );
        let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
        let package = crate::support::package_with_contributions(
            &worker,
            Contributions::from([contribution]),
        );
        let (cache, runtime) = setup_package(
            vec![InstanceFixture {
                id: "fast-settings",
                configuration: json!({
                    "mode":"settings", "expected_settings":wire, "settings":updated,
                    "expected_fast_source": if mode == FastMode::Default { Value::Null } else {
                        json!({"instance_id":"seed", "order":1, "value":mode.as_str()})
                    },
                }),
                bindings: vec![binding(
                    MIDDLEWARE_CONTRIBUTION,
                    "request",
                    0,
                    PluginFailurePolicy::Reject,
                )],
            }],
            package,
        )
        .await;
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let next = (Downstream {
            calls: Arc::default(),
            closes: Arc::default(),
            reads: Arc::default(),
            error: false,
        })
        .into_next();
        let response = plan
            .handle(
                middleware_context(ClientTransport::HttpJson),
                MiddlewareRequest::new(
                    "openai",
                    middleware_headers(),
                    Bytes::from_static(br#"{"input":"hello"}"#),
                )
                .with_settings(settings),
                gateway_core::middleware::compose(Vec::new(), move |request: MiddlewareRequest| {
                    Box::pin(async move {
                        let settings = request.settings().unwrap();
                        assert_eq!(settings.execution_values().unwrap().fast_mode, expected);
                        assert_eq!(
                            settings.execution_values().unwrap().timeout_ms,
                            Some(90_000)
                        );
                        let sources = settings.inspect();
                        let fast_source = &sources["execution"]["fast_mode"];
                        if mode == FastMode::Default && expected == FastMode::Default {
                            assert!(fast_source.is_null());
                        } else {
                            assert_eq!(
                                fast_source["instance_id"],
                                if mode == expected {
                                    "seed"
                                } else {
                                    "fast-settings"
                                }
                            );
                            assert_eq!(fast_source["value"], expected.as_str());
                        }
                        assert_eq!(
                            sources["execution"]["timeout_ms"]["instance_id"],
                            "fast-settings"
                        );
                        next.run(request).await
                    })
                }),
            )
            .await
            .unwrap();
        let (_, _, _, mut body, _) = response.into_parts();
        while body.next_frame().await.unwrap().is_some() {}
        body.close().await;
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
        super::super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn unsupported_contract_starts_and_response_errors_do_not_stop_the_instance() {
    use gateway_admin::{
        model::plugins::instances::PluginInstanceRuntimeStatus,
        ports::plugins::PluginRuntimeDiagnostics as _,
    };
    for failure in ["invalid_head", "crash", "business_error"] {
        let mut declaration = crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        );
        declaration.1.version = 2;
        let (cache, store, runtime) =
            super::super::setup_with_contributions(Contributions::from([declaration])).await;
        {
            let mut snapshot = store.snapshot.lock().unwrap();
            let instance = &mut snapshot.instances[0];
            instance.bindings = vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "request",
                0,
                PluginFailurePolicy::Delegate,
            )];
            instance.configuration = json!({
                "middleware_runtime_failure":failure,
                "middleware_fault_before_next": failure == "business_error",
            });
            let mut healthy = instance.clone();
            healthy.id = "healthy".into();
            healthy.bindings[0].order = 1;
            healthy.configuration = json!({});
            snapshot.instances.push(healthy);
        }
        let generation = prepare(&runtime).await;
        assert!(generation.is_ready(), "版本警告允许随宿主启动");
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let response = plan
                .handle(
                    middleware_context(ClientTransport::HttpJson),
                    MiddlewareRequest::new(
                        "openai",
                        middleware_headers(),
                        Bytes::from_static(br#"{"input":"hello"}"#),
                    ),
                    (Downstream {
                        calls: calls.clone(),
                        closes: Arc::default(),
                        reads: Arc::default(),
                        error: false,
                    })
                    .into_next(),
                )
                .await
                .unwrap();
            let (_, _, _, mut body, _) = response.into_parts();
            while body.next_frame().await.unwrap().is_some() {}
            body.close().await;
            assert!(generation.can_serve(), "插件错误不能改变宿主服务状态");
        }
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(snapshot.instances.iter().all(|instance| instance.enabled));
        let diagnostics = runtime
            .runtime_diagnostics(&snapshot, Some(1), Some(&generation))
            .await
            .unwrap();
        assert_eq!(
            diagnostics["healthy"].status,
            PluginInstanceRuntimeStatus::Running
        );
        let failed = &diagnostics["instance-one"];
        if failure != "crash" {
            assert_eq!(failed.status, PluginInstanceRuntimeStatus::Running);
            assert!(generation.is_ready());
        } else {
            assert_eq!(failed.status, PluginInstanceRuntimeStatus::Faulted);
        }
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
        super::super::wait_until_empty(cache.path()).await;
    }
}
