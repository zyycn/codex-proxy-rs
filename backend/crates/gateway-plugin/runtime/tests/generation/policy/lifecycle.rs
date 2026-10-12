//! 验证中间件续体的调用预算、取消回收及跨集合冻结计划

use super::*;
use gateway_admin::ports::plugins::PluginPreparation;

#[tokio::test]
async fn partial_update_preserves_inflight_middleware_and_uses_new_configuration_for_new_calls() {
    let (cache, store, runtime) = super::super::setup_with_contributions(Contributions::from([
        crate::support::contribution(Capability::Middleware, vec![Stage::Request], vec![], vec![]),
    ]))
    .await;
    let markers = tempfile::tempdir().unwrap();
    let mut snapshot = store.snapshot.lock().unwrap().clone();
    let template = snapshot.instances[0].clone();
    snapshot.instances = ["a", "b", "c"]
        .into_iter()
        .enumerate()
        .map(|(order, id)| {
            let mut instance = template.clone();
            instance.id = id.into();
            instance.configuration = serde_json::json!({"startup_marker": markers.path().join(id)});
            instance.bindings = vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "request",
                order as i32,
                PluginFailurePolicy::Reject,
            )];
            instance
        })
        .collect();
    let old = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();
    let old_plan = runtime.execution_registry().middleware(&old).unwrap();
    assert!(runtime.policy_registry().resolve(&old).is_none());
    let (started, waiting) = tokio::sync::oneshot::channel();
    let (resume, blocked) = tokio::sync::oneshot::channel();
    let inflight = tokio::spawn(async move {
        old_plan
            .handle(
                middleware_context(ClientTransport::HttpJson),
                MiddlewareRequest::new(
                    "openai",
                    middleware_headers(),
                    Bytes::from_static(br#"{"input":"hello"}"#),
                ),
                gateway_core::middleware::compose(Vec::new(), move |request| {
                    Box::pin(async move {
                        started.send(()).unwrap();
                        blocked.await.unwrap();
                        Downstream {
                            calls: Arc::default(),
                            closes: Arc::default(),
                            reads: Arc::default(),
                            error: false,
                        }
                        .into_next()
                        .run(request)
                        .await
                    })
                }),
            )
            .await
    });
    waiting.await.unwrap();
    snapshot.config_revision = Revision::new(2).unwrap();
    snapshot.instances[1].revision = snapshot.config_revision;
    snapshot.instances[1].configuration["middleware_map_body"] = true.into();
    let new = PluginPreparation::prepare(&runtime, snapshot)
        .await
        .unwrap();
    drop(old);
    let new_plan = runtime.execution_registry().middleware(&new).unwrap();
    let changed = new_plan
        .handle(
            middleware_context(ClientTransport::HttpJson),
            MiddlewareRequest::new(
                "openai",
                middleware_headers(),
                Bytes::from_static(br#"{"input":"hello"}"#),
            ),
            Downstream {
                calls: Arc::default(),
                closes: Arc::default(),
                reads: Arc::default(),
                error: false,
            }
            .into_next(),
        )
        .await;
    let (_, _, _, mut changed_body, _) = changed.unwrap().into_parts();
    assert_eq!(
        changed_body
            .next_frame()
            .await
            .unwrap()
            .unwrap()
            .into_bytes(),
        Bytes::from_static(b"{\"downstream\":true} ")
    );
    changed_body.close().await;
    resume.send(()).unwrap();
    let response = inflight
        .await
        .unwrap()
        .expect("the old request completes through A/B/C callbacks");
    let (_, _, _, mut body, _) = response.into_parts();
    assert_eq!(
        body.next_frame().await.unwrap().unwrap().into_bytes(),
        Bytes::from_static(br#"{"downstream":true}"#)
    );
    body.close().await;
    assert!(new.is_ready());
    for (id, count) in [("a", 1), ("b", 2), ("c", 1)] {
        assert_eq!(
            std::fs::read_to_string(markers.path().join(id))
                .unwrap()
                .lines()
                .count(),
            count
        );
    }
    drop(new_plan);
    drop(new);
    super::super::wait_until_empty(cache.path()).await;
}

async fn sdk_budget_runtime(stage: Stage, count: usize) -> (tempfile::TempDir, PluginRuntime) {
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![stage],
            vec!["openai".into()],
            vec!["openai".into()],
        )]),
    );
    let mount = if stage == Stage::Attempt {
        "attempt"
    } else {
        "request"
    };
    let instances = ["budget-outer", "budget-inner"]
        .into_iter()
        .take(count)
        .enumerate()
        .map(|(order, id)| InstanceFixture {
            id,
            configuration: serde_json::json!({"mode":"map", "attempt":stage == Stage::Attempt}),
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                mount,
                order as i32,
                PluginFailurePolicy::Reject,
            )],
        })
        .collect();
    setup_package_with_limits(
        instances,
        package,
        RpcLimits {
            maximum_call_timeout: Duration::from_millis(300),
            ..RpcLimits::default()
        },
    )
    .await
}

fn budget_request() -> MiddlewareRequest {
    MiddlewareRequest::new(
        "openai",
        middleware_headers(),
        Bytes::from_static(br#"{"input":"hello"}"#),
    )
}

#[tokio::test]
async fn nested_sdk_middleware_waits_beyond_budget_and_can_read_response_callbacks() {
    for stage in [Stage::Request, Stage::Attempt] {
        let (cache, runtime) = sdk_budget_runtime(stage, 2).await;
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let context = if stage == Stage::Attempt {
            attempt_middleware_context(Arc::default())
        } else {
            middleware_context_with_deadline(ClientTransport::HttpJson, Default::default())
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let downstream = Downstream {
            calls: calls.clone(),
            closes: Arc::default(),
            reads: Arc::default(),
            error: false,
        };
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            plan.handle(
                context,
                budget_request(),
                gateway_core::middleware::compose(Vec::new(), move |request| {
                    Box::pin(async move {
                        tokio::time::sleep(Duration::from_millis(900)).await;
                        downstream.into_next().run(request).await
                    })
                }),
            ),
        )
        .await
        .unwrap()
        .expect("host next must not consume either plugin's processing budget");
        let (_, status, _, mut body, _) = response.into_parts();
        assert_eq!(status, 200);
        // 正文改写还会发起新的 body_read，证明 callback 有效期也随暂停恢复
        let frame = body.next_frame().await.unwrap().unwrap();
        assert_eq!(
            frame.into_bytes(),
            Bytes::from_static(b"{\"downstream\":true}  ")
        );
        assert!(body.next_frame().await.unwrap().is_none());
        body.close().await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(generation.is_ready());
        drop(plan);
        drop(generation);
        super::super::wait_until_empty(cache.path()).await;
    }
}

struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn sdk_waiting_next_obeys_request_deadline_and_client_cancellation() {
    for cancel_client in [false, true] {
        let (cache, runtime) = sdk_budget_runtime(Stage::Request, 1).await;
        let generation = prepare(&runtime).await;
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let deadline = if cancel_client {
            Default::default()
        } else {
            (SystemTime::now() + Duration::from_millis(900)).into()
        };
        let context = middleware_context_with_deadline(ClientTransport::HttpJson, deadline);
        let cancellation = context.cancellation().clone();
        let (started, entered) = tokio::sync::oneshot::channel();
        let (closed, dropped) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            plan.handle(
                context,
                budget_request(),
                gateway_core::middleware::compose(Vec::new(), move |_| {
                    Box::pin(async move {
                        let _closed = NotifyOnDrop(Some(closed));
                        let _ = started.send(());
                        futures::future::pending().await
                    })
                }),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !pending.is_finished(),
            "plugin budget must stay paused while waiting for next"
        );
        if cancel_client {
            cancellation.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        match result {
            Err(MiddlewareError::Fault) if cancel_client => {}
            Err(MiddlewareError::Gateway(error)) if !cancel_client => {
                assert_eq!(error.kind(), gateway_core::error::GatewayErrorKind::Timeout);
            }
            _ => panic!("request termination must end the suspended middleware"),
        }
        tokio::time::timeout(Duration::from_secs(2), dropped)
            .await
            .unwrap()
            .unwrap();
        // 取消单次调用后，同一进程仍能完成新的中间件及正文回调
        let plan = runtime
            .execution_registry()
            .middleware(&generation)
            .unwrap();
        let response = plan
            .handle(
                middleware_context(ClientTransport::HttpJson),
                budget_request(),
                Downstream {
                    calls: Arc::default(),
                    closes: Arc::default(),
                    reads: Arc::default(),
                    error: false,
                }
                .into_next(),
            )
            .await
            .unwrap();
        let (_, _, _, mut body, _) = response.into_parts();
        assert!(body.next_frame().await.unwrap().is_some());
        body.close().await;
        assert!(generation.is_ready());
        drop(plan);
        drop(generation);
        super::super::wait_until_empty(cache.path()).await;
    }
}
