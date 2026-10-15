//! 验证 next 等待不占执行名额，硬上限拒绝请求且不跳过插件

use super::*;
use gateway_core::error::GatewayErrorKind;

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
async fn http_waiting_next_releases_execution_for_delegate_plugin() {
    assert_capacity(PluginFailurePolicy::Delegate, RpcLimits::default(), false).await;
}

#[tokio::test]
async fn http_waiting_next_releases_execution_for_reject_plugin() {
    assert_capacity(PluginFailurePolicy::Reject, RpcLimits::default(), false).await;
}

#[tokio::test]
async fn http_in_flight_limit_never_delegates_and_recovers_after_cancellation() {
    for policy in [PluginFailurePolicy::Delegate, PluginFailurePolicy::Reject] {
        assert_capacity(
            policy,
            RpcLimits {
                maximum_calls: 64,
                ..Default::default()
            },
            true,
        )
        .await;
    }
}

async fn assert_capacity(policy: PluginFailurePolicy, limits: RpcLimits, saturated: bool) {
    const CONCURRENT_CALLS: usize = 64;
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
            .header("authorization", "Bearer fixture-secret")
            .body(core::empty_body())
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            plan.handle_http(
                context(),
                request,
                compose(Vec::new(), move |request: core::Request| {
                    assert!(!saturated, "容量耗尽时不得执行下游");
                    assert_eq!(request.uri(), "/rewritten?encoded=%2F");
                    Box::pin(async { Ok(core::Response::new(core::empty_body())) })
                }),
            ),
        )
        .await
        .expect("下游等待不应阻塞其它请求");
        if saturated {
            assert!(matches!(result, Err(MiddlewareError::Gateway(error))
                if error.kind() == GatewayErrorKind::MiddlewareCapacityUnavailable));
        } else {
            assert_eq!(result.unwrap().headers()["x-http-plugin"], "active");
        }
    }

    if saturated {
        slow_calls.abort_all();
        while let Some(result) = slow_calls.join_next().await {
            assert!(matches!(result, Err(error) if error.is_cancelled()));
        }
    }
    release.add_permits(CONCURRENT_CALLS);
    while let Some(result) = slow_calls.join_next().await {
        let response = result.unwrap().unwrap();
        assert_eq!(response.headers()["x-http-plugin"], "active");
    }
    // 取消确认到达前仍持有在途名额，探测只允许遇到明确的暂时过载
    let response = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let result = plan
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
                .await;
            match result {
                Ok(response) => break response,
                Err(MiddlewareError::Gateway(error))
                    if error.kind() == GatewayErrorKind::MiddlewareCapacityUnavailable =>
                {
                    tokio::task::yield_now().await
                }
                Err(error) => panic!("unexpected error: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(response.headers()["x-http-plugin"], "active");
    drop(response);
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
    crate::generation::wait_until_empty(cache.path()).await;
}
