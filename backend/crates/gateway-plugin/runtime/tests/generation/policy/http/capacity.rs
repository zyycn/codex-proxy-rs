//! 验证默认容量承载 64 个慢调用，饱和时不跳过插件且释放后恢复

use super::*;
use gateway_plugin_runtime::RpcError;

fn context() -> core::Context {
    core::Context {
        plugin_instance_id: None,
        call_id: uuid::Uuid::new_v4().to_string(),
        parent_call_id: None,
        extensions: Default::default(),
        plan: None,
        request_id: uuid::Uuid::new_v4().to_string(),
        cancellation: CancellationToken::new(),
    }
}

fn plugin_request() -> core::Request {
    ::http::Request::builder()
        .uri("/v1/responses")
        .header("authorization", "Bearer fixture-secret")
        .body(core::empty_body())
        .unwrap()
}

#[tokio::test]
async fn http_default_capacity_runs_64_calls_without_skipping_delegate_plugin() {
    assert_saturated_policy(PluginFailurePolicy::Delegate).await;
}

#[tokio::test]
async fn http_default_capacity_runs_64_calls_without_skipping_reject_plugin() {
    assert_saturated_policy(PluginFailurePolicy::Reject).await;
}

async fn assert_saturated_policy(policy: PluginFailurePolicy) {
    const CONCURRENT_CALLS: usize = 64;
    let limits = RpcLimits::default();
    let (cache, runtime, generation, plan) =
        http_plugin_with_policy("passthrough", limits, policy).await;
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (started, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let mut slow_calls = tokio::task::JoinSet::new();
    for index in 0..CONCURRENT_CALLS {
        let plan = plan.clone();
        let release = release.clone();
        let started = started.clone();
        slow_calls.spawn(async move {
            plan.handle_http(
                context(),
                plugin_request(),
                compose(Vec::new(), move |request: core::Request| {
                    assert_eq!(request.uri(), "/rewritten?encoded=%2F");
                    let release = release.clone();
                    Box::pin(async move {
                        started.send(()).unwrap();
                        release.acquire().await.unwrap().forget();
                        Ok(core::Response::new(core::empty_body()))
                    })
                }),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered.recv())
            .await
            .unwrap_or_else(|_| panic!("第 {} 个调用未能进入下游", index + 1))
            .unwrap();
    }

    for uri in [
        "/healthz",
        "/assets/index.js",
        "/api/settings",
        "/v1/responses",
    ] {
        let request = ::http::Request::builder()
            .uri(uri)
            .body(core::empty_body())
            .unwrap();
        let result = plan
            .handle_http(
                context(),
                request,
                compose(Vec::new(), |_: core::Request| {
                    panic!("容量耗尽时不得跳过插件执行下游")
                }),
            )
            .await;
        assert!(matches!(result, Err(MiddlewareError::Remote { source, .. })
            if matches!(source.downcast_ref::<RpcError>(), Some(RpcError::Capacity))));
    }

    release.add_permits(CONCURRENT_CALLS);
    while let Some(result) = slow_calls.join_next().await {
        let response = result.unwrap().unwrap();
        assert_eq!(response.headers()["x-http-plugin"], "active");
    }
    let response = plan
        .handle_http(
            context(),
            plugin_request(),
            compose(Vec::new(), |request: core::Request| {
                Box::pin(async move {
                    assert_eq!(request.uri(), "/rewritten?encoded=%2F");
                    Ok(core::Response::new(core::empty_body()))
                })
            }),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["x-http-plugin"], "active");
    drop(response);
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
    crate::generation::wait_until_empty(cache.path()).await;
}
