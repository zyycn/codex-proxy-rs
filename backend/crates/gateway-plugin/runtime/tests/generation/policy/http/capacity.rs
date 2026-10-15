//! 验证 HTTP 插件槽位饱和时的绑定策略、原始请求保留与调用恢复

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
async fn saturated_http_delegate_preserves_requests_and_recovers_after_slow_calls() {
    assert_saturated_policy(PluginFailurePolicy::Delegate).await;
}

#[tokio::test]
async fn saturated_http_reject_does_not_run_downstream() {
    assert_saturated_policy(PluginFailurePolicy::Reject).await;
}

#[tokio::test]
async fn http_delegate_does_not_replay_plugin_capacity_errors() {
    for (mode, expected_calls) in [("capacity_before_next", 0), ("capacity_after_next", 1)] {
        let (cache, runtime, generation, plan) =
            http_plugin_with_policy(mode, RpcLimits::default(), PluginFailurePolicy::Delegate)
                .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let result = plan
            .handle_http(
                context(),
                plugin_request(),
                compose(Vec::new(), move |_: core::Request| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(core::Response::new(core::empty_body())) })
                }),
            )
            .await;
        assert!(matches!(result, Err(MiddlewareError::Remote { source, .. })
            if matches!(source.downcast_ref::<RpcError>(), Some(RpcError::Remote(fault))
                if fault.code == gateway_plugin_sdk::ErrorCode::Capacity)));
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
        crate::generation::wait_until_empty(cache.path()).await;
    }
}

async fn assert_saturated_policy(policy: PluginFailurePolicy) {
    let limits = RpcLimits::default();
    let (cache, runtime, generation, plan) =
        http_plugin_with_policy("passthrough", limits, policy.clone()).await;
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (started, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let mut slow_calls = tokio::task::JoinSet::new();
    for _ in 0..limits.maximum_calls {
        let plan = plan.clone();
        let release = release.clone();
        let started = started.clone();
        slow_calls.spawn(async move {
            plan.handle_http(
                context(),
                plugin_request(),
                compose(Vec::new(), move |_: core::Request| {
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
            .unwrap()
            .unwrap();
    }

    for uri in [
        "/healthz",
        "/assets/index.js",
        "/api/settings",
        "/v1/responses",
    ] {
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let mut trailers = ::http::HeaderMap::new();
        trailers.append("x-end", "one".parse().unwrap());
        trailers.append("x-end", "two".parse().unwrap());
        let mut frames = vec![
            Frame::data(Bytes::from_static(b"original body")),
            Frame::trailers(trailers),
        ]
        .into_iter();
        let body = StreamBody::new(futures::stream::poll_fn(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::Ready(frames.next().map(Ok::<_, std::io::Error>))
        }))
        .map_err(|error| Box::new(error) as _)
        .boxed_unsync();
        let mut request = ::http::Request::builder()
            .method("POST")
            .uri(uri)
            .version(::http::Version::HTTP_2)
            .header("x-duplicate", "one")
            .header("x-duplicate", "two")
            .body(body)
            .unwrap();
        request.extensions_mut().insert(123_u32);
        request.extensions_mut().insert(core::Settings {
            runtime: None,
            timeout: Some(Duration::from_secs(7)),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let result = plan
            .handle_http(
                context(),
                request,
                compose(Vec::new(), move |request: core::Request| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        assert_eq!(request.uri(), uri);
                        assert_eq!(request.method(), "POST");
                        assert_eq!(request.version(), ::http::Version::HTTP_2);
                        assert_eq!(request.headers().get_all("x-duplicate").iter().count(), 2);
                        assert_eq!(request.extensions().get::<u32>(), Some(&123));
                        assert_eq!(
                            request
                                .extensions()
                                .get::<core::Settings>()
                                .unwrap()
                                .timeout,
                            Some(Duration::from_secs(7)),
                        );
                        let mut response = core::Response::new(request.into_body());
                        response.extensions_mut().insert(456_u32);
                        Ok(response)
                    })
                }),
            )
            .await;
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        if policy == PluginFailurePolicy::Delegate {
            let response = result.expect("容量不足时应按绑定委托原始请求");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(response.extensions().get::<u32>(), Some(&456));
            assert!(!response.headers().contains_key("x-http-plugin"));
            let body = response.into_body().collect().await.unwrap();
            assert_eq!(body.trailers().unwrap().get_all("x-end").iter().count(), 2);
            assert_eq!(body.to_bytes(), "original body");
        } else {
            assert!(matches!(result, Err(MiddlewareError::Remote { source, .. })
                if matches!(source.downcast_ref::<RpcError>(), Some(RpcError::Capacity))));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    let cancelled = context();
    cancelled.cancellation.cancel();
    assert!(
        plan.handle_http(
            cancelled,
            plugin_request(),
            compose(Vec::new(), |_: core::Request| {
                panic!("已取消请求不能因容量不足而委托")
            }),
        )
        .await
        .is_err()
    );

    release.add_permits(limits.maximum_calls);
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
