//! 验证旧中间件的 Fast 设置适配与不兼容插件的实例故障隔离

use super::*;
use gateway_core::{policy::ClientPolicy, settings::RequestSettings};
use serde_json::{Value, json};

#[tokio::test]
async fn legacy_middleware_preserves_three_state_fast_and_projects_sources() {
    for (mode, write, expected) in [
        (FastMode::Default, false, FastMode::Default),
        (FastMode::Enabled, false, FastMode::Enabled),
        (FastMode::Disabled, true, FastMode::Disabled),
        (FastMode::Enabled, true, FastMode::Disabled),
        (FastMode::Disabled, false, FastMode::Default),
    ] {
        let snapshot = gateway_core::routing::RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            gateway_core::settings::SettingsValues::new(
                1,
                0,
                "smart",
                Default::default(),
                None,
                None,
            ),
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
        let mut wire = serde_json::to_value(&seed).unwrap();
        wire.as_object_mut().unwrap().remove("fast_mode");
        wire["disable_fast"] = json!(mode == FastMode::Disabled);
        let mut updated = wire.clone();
        updated["disable_fast"] = json!(write);
        updated["timeout_ms"] = json!(90_000);
        let mut contribution = crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        );
        contribution.1.version = 3;
        let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
        let package = crate::support::package_with_contributions(
            &worker,
            Contributions::from([contribution]),
        );
        let (cache, runtime) = setup_package(vec![InstanceFixture {
            id: "legacy-settings",
            configuration: json!({
                "mode":"settings", "middleware_version":3, "expected_settings":wire, "settings":updated,
                "expected_legacy_source": if mode == FastMode::Default { Value::Null } else {
                    json!({"instance_id":"seed", "order":1, "value":mode == FastMode::Disabled})
                },
            }),
            bindings: vec![binding(MIDDLEWARE_CONTRIBUTION,"request",0,PluginFailurePolicy::Reject)],
        }], package).await;
        let generation = prepare(&runtime).await;
        let plan = runtime.middleware_registry().resolve(&generation).unwrap();
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
                        assert!(sources["execution"].get("disable_fast").is_none());
                        if mode == FastMode::Enabled && !write {
                            assert_eq!(sources["execution"]["fast_mode"]["instance_id"], "seed");
                        }
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
async fn unsupported_contract_starts_and_runtime_faults_only_stop_the_failed_instance() {
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
        let plan = runtime.middleware_registry().resolve(&generation).unwrap();
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
        if failure == "business_error" {
            assert_eq!(failed.status, PluginInstanceRuntimeStatus::Running);
            assert!(generation.is_ready());
        } else {
            assert_eq!(failed.status, PluginInstanceRuntimeStatus::Faulted);
            let reason = failed.failure.as_ref().unwrap();
            if failure == "invalid_head" {
                assert_eq!(reason.code, "invalid_response");
                assert!(reason.message.contains("请求中间件"));
                assert!(!reason.message.contains("sensitive"));
            }
        }
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
        super::super::wait_until_empty(cache.path()).await;
    }
}
