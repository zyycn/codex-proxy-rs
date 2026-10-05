//! 验证真实插件 HTTP 中间件的请求改写、流式转换与短路响应

mod dispatch;
mod websocket;

use super::*;
use gateway_core::middleware::{compose, http as core};
use http_body::Frame;
use http_body_util::{BodyExt as _, StreamBody};

async fn http_plugin(
    mode: &str,
) -> (
    tempfile::TempDir,
    PluginRuntime,
    ExtensionSetReference,
    gateway_core::engine::middleware::FrozenMiddlewarePlan,
) {
    http_plugin_with_limits(mode, RpcLimits::default()).await
}

async fn http_plugin_with_limits(
    mode: &str,
    limits: RpcLimits,
) -> (
    tempfile::TempDir,
    PluginRuntime,
    ExtensionSetReference,
    gateway_core::engine::middleware::FrozenMiddlewarePlan,
) {
    let worker = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-middleware")).unwrap();
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Http],
            vec!["http".into()],
            vec!["http".into()],
        )]),
    );
    let (cache, runtime) = setup_package_with_limits(
        vec![InstanceFixture {
            id: "http-plugin",
            configuration: serde_json::json!({"http":true,"mode":mode}),
            bindings: vec![binding(
                MIDDLEWARE_CONTRIBUTION,
                "http",
                0,
                PluginFailurePolicy::Reject,
            )],
        }],
        package,
        limits,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    assert!(plan.has_http());
    (cache, runtime, generation, plan)
}

#[tokio::test]
async fn real_http_plugin_rewrites_full_request_without_eager_body_reads() {
    let (cache, runtime, generation, plan) = http_plugin("passthrough").await;
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    let mut frames = vec![Frame::data(Bytes::from_static(b"opaque request"))].into_iter();
    let body = StreamBody::new(futures::stream::poll_fn(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(frames.next().map(Ok::<_, std::io::Error>))
    }))
    .map_err(|error| Box::new(error) as _)
    .boxed_unsync();
    let mut request = ::http::Request::builder()
        .uri("/custom/route")
        .header("authorization", "Bearer fixture-secret")
        .header("x-duplicate", "one")
        .header("x-duplicate", "two")
        .body(body)
        .unwrap();
    request.extensions_mut().insert(core::Settings {
        runtime: None,
        timeout: Some(Duration::from_secs(1)),
    });
    request.extensions_mut().insert(123_u32);
    let next = compose(Vec::new(), |request: core::Request| {
        Box::pin(async move {
            assert_eq!(request.uri(), "/rewritten?encoded=%2F");
            assert_eq!(request.method(), "PATCH");
            assert_eq!(request.headers().get_all("x-duplicate").iter().count(), 2);
            assert_eq!(request.extensions().get::<u32>(), Some(&123));
            assert_eq!(
                request
                    .extensions()
                    .get::<core::Settings>()
                    .unwrap()
                    .timeout,
                None
            );
            let mut response = core::Response::new(request.into_body());
            response.extensions_mut().insert(456_u32);
            Ok(response)
        })
    });
    let response = plan
        .handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                plan: None,
                request_id: "http-request".into(),
                cancellation: CancellationToken::new(),
            },
            request,
            next,
        )
        .await
        .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(response.headers()["x-http-plugin"], "active");
    assert_eq!(response.extensions().get::<u32>(), Some(&456));
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "opaque request"
    );
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
    crate::generation::wait_until_empty(cache.path()).await;
}

#[tokio::test]
async fn real_http_plugin_maps_stream_and_preserves_trailers() {
    let (_cache, runtime, generation, plan) = http_plugin("map").await;
    let request = ::http::Request::builder()
        .uri("/any")
        .header("authorization", "Bearer fixture-secret")
        .body(core::empty_body())
        .unwrap();
    let next = compose(Vec::new(), |_: core::Request| {
        Box::pin(async {
            let mut trailers = ::http::HeaderMap::new();
            trailers.append("x-end", "one".parse().unwrap());
            trailers.append("x-end", "two".parse().unwrap());
            let body = StreamBody::new(futures::stream::iter(vec![
                Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"first"))),
                Ok(Frame::data(Bytes::from_static(b"second"))),
                Ok(Frame::trailers(trailers)),
            ]))
            .map_err(|error| Box::new(error) as _)
            .boxed_unsync();
            Ok(core::Response::new(body))
        })
    });
    let response = plan
        .handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                plan: None,
                request_id: "http-stream".into(),
                cancellation: CancellationToken::new(),
            },
            request,
            next,
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap();
    assert_eq!(body.trailers().unwrap().get_all("x-end").iter().count(), 2);
    assert_eq!(body.to_bytes(), "FIRSTSECOND");
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn real_http_plugin_can_respond_to_unknown_route_without_calling_terminal() {
    let (_cache, runtime, generation, plan) = http_plugin("short").await;
    let request = ::http::Request::builder()
        .uri("/plugin-owned")
        .body(core::empty_body())
        .unwrap();
    let next = compose(Vec::new(), |_: core::Request| {
        Box::pin(async { panic!("short circuit cannot route") })
    });
    let response = plan
        .handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                plan: None,
                request_id: "http-short".into(),
                cancellation: CancellationToken::new(),
            },
            request,
            next,
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 418);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "plugin route"
    );
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn transformed_http_body_respects_a_smaller_host_credit_window() {
    let (_cache, runtime, generation, plan) = http_plugin_with_limits(
        "map",
        RpcLimits {
            maximum_buffered_body_bytes: 8192,
            ..Default::default()
        },
    )
    .await;
    let request = ::http::Request::builder()
        .uri("/large-frame")
        .header("authorization", "Bearer fixture-secret")
        .body(core::empty_body())
        .unwrap();
    let next = compose(Vec::new(), |_: core::Request| {
        Box::pin(async {
            // 单个源帧大于窗口，HTTP 转换应按可用信用拆分，而不要求业务更改源帧大小
            Ok(core::Response::new(
                http_body_util::Full::new(Bytes::from(vec![b'a'; 65536]))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            ))
        })
    });
    let response = plan
        .handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                plan: None,
                request_id: "small-window".into(),
                cancellation: CancellationToken::new(),
            },
            request,
            next,
        )
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body, Bytes::from(vec![b'A'; 65536]));
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn real_http_plugin_transforms_upload_with_early_and_late_response_headers() {
    for early_headers in [false, true] {
        let (_cache, runtime, generation, plan) = http_plugin("upload").await;
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let frames = (0..96).map(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>(Frame::data(Bytes::from(vec![b'a'; 65536])))
        });
        let body = StreamBody::new(futures::stream::iter(frames))
            .map_err(|error| Box::new(error) as _)
            .boxed_unsync();
        let request = ::http::Request::builder()
            .uri("/upload")
            .header("authorization", "Bearer fixture-secret")
            .body(body)
            .unwrap();
        let next = compose(Vec::new(), move |request: core::Request| {
            Box::pin(async move {
                if early_headers {
                    Ok(core::Response::new(request.into_body()))
                } else {
                    let bytes = request.into_body().collect().await.unwrap().to_bytes();
                    assert_eq!(bytes.len(), 96 * 65536);
                    assert!(bytes.iter().all(|byte| *byte == b'A'));
                    Ok(core::Response::new(
                        http_body_util::Full::new(bytes)
                            .map_err(|never| match never {})
                            .boxed_unsync(),
                    ))
                }
            })
        });
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            plan.handle_http(
                core::Context {
                    plugin_instance_id: None,
                    call_id: "http-call".into(),
                    parent_call_id: None,
                    extensions: Default::default(),
                    plan: None,
                    request_id: "upload".into(),
                    cancellation: CancellationToken::new(),
                },
                request,
                next,
            ),
        )
        .await
        .unwrap()
        .unwrap_or_else(|error| {
            if let MiddlewareError::Remote { source, .. } = &error
                && let Some(gateway_plugin_runtime::RpcError::Remote(fault)) =
                    source.downcast_ref::<gateway_plugin_runtime::RpcError>()
            {
                panic!("upload middleware: {}", fault.message);
            }
            panic!("upload middleware: {error:?}");
        });
        if early_headers {
            assert!(reads.load(Ordering::SeqCst) < 96, "响应头不能等待完整上传");
        }
        let body = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
            .await
            .unwrap()
            .unwrap_or_else(|error| {
                if let Some(MiddlewareError::Remote { source, .. }) =
                    error.downcast_ref::<MiddlewareError>()
                    && let Some(gateway_plugin_runtime::RpcError::Remote(fault)) =
                        source.downcast_ref::<gateway_plugin_runtime::RpcError>()
                {
                    panic!("upload response (early={early_headers}): {}", fault.message);
                }
                panic!("upload response (early={early_headers}): {error:?}");
            })
            .to_bytes();
        assert_eq!(body.len(), 96 * 65536);
        assert!(body.iter().all(|byte| *byte == b'A'));
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn early_rejection_does_not_wait_for_or_mask_an_unconsumed_upload() {
    let (_cache, runtime, generation, plan) = http_plugin("upload").await;
    let body = StreamBody::new(futures::stream::repeat_with(|| {
        Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"ignored")))
    }))
    .map_err(|error| Box::new(error) as _)
    .boxed_unsync();
    let request = ::http::Request::builder()
        .uri("/upload")
        .header("authorization", "Bearer fixture-secret")
        .body(body)
        .unwrap();
    let next = compose(Vec::new(), move |_: core::Request| {
        Box::pin(async {
            let mut response = core::Response::new(core::empty_body());
            *response.status_mut() = ::http::StatusCode::UNAUTHORIZED;
            Ok(response)
        })
    });
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        plan.handle_http(
            core::Context {
                plugin_instance_id: None,
                call_id: "http-call".into(),
                parent_call_id: None,
                extensions: Default::default(),
                plan: None,
                request_id: "upload-rejected".into(),
                cancellation: CancellationToken::new(),
            },
            request,
            next,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 401);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

struct UploadLifetime(Arc<std::sync::atomic::AtomicBool>);
impl futures::Stream for UploadLifetime {
    type Item = Result<Frame<Bytes>, std::io::Error>;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
            b"still uploading",
        )))))
    }
}
impl Drop for UploadLifetime {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn dropping_or_cancelling_a_response_releases_its_active_upload() {
    for cancel in [false, true] {
        let (_cache, runtime, generation, plan) = http_plugin("upload").await;
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let body = StreamBody::new(UploadLifetime(dropped.clone()))
            .map_err(|error| Box::new(error) as _)
            .boxed_unsync();
        let request = ::http::Request::builder()
            .uri("/upload")
            .header("authorization", "Bearer fixture-secret")
            .body(body)
            .unwrap();
        let cancellation = CancellationToken::new();
        let response = plan
            .handle_http(
                core::Context {
                    plugin_instance_id: None,
                    call_id: "http-call".into(),
                    parent_call_id: None,
                    extensions: Default::default(),
                    plan: None,
                    request_id: "cancel-upload".into(),
                    cancellation: cancellation.clone(),
                },
                request,
                compose(Vec::new(), |request: core::Request| {
                    Box::pin(async { Ok(core::Response::new(request.into_body())) })
                }),
            )
            .await
            .unwrap();
        let mut body = response.into_body();
        assert!(body.frame().await.unwrap().is_ok());
        assert!(!dropped.load(Ordering::SeqCst));
        if cancel {
            cancellation.cancel();
            assert!(body.frame().await.unwrap().is_err());
        }
        drop(body);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("upload owner must drop with its response");
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
    }
}

/// 显式以 release 运行，避免把调试构建或进程启动时间计入请求开销
#[cfg(not(debug_assertions))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with --release --ignored --nocapture for the HTTP middleware baseline"]
async fn http_onion_performance_baseline() {
    let worker = std::fs::read(
        std::env::var_os("CPR_PLUGIN_BENCH_WORKER")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_gateway-plugin-test-middleware").into()),
    )
    .unwrap();
    let selected = std::env::var("CPR_PLUGIN_BENCH_CASE").ok();
    assert!(selected.as_deref().is_none_or(|case| {
        ["native", "passthrough", "compose-three", "transform"].contains(&case)
    }));
    let package = crate::support::package_with_contributions(
        &worker,
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Http],
            vec!["http".into()],
            vec!["http".into()],
        )]),
    );
    for (label, mode, count) in [
        ("native", "identity", 0),
        ("passthrough", "identity", 1),
        ("compose-three", "identity", 3),
        ("transform", "map", 1),
    ] {
        if selected.as_deref().is_some_and(|case| case != label) {
            continue;
        }
        let instances = ["bench-a", "bench-b", "bench-c"]
            .into_iter()
            .take(count)
            .enumerate()
            .map(|(index, id)| InstanceFixture {
                id,
                configuration: serde_json::json!({"http":true,"mode":mode}),
                bindings: vec![binding(
                    MIDDLEWARE_CONTRIBUTION,
                    "http",
                    index as i32,
                    PluginFailurePolicy::Reject,
                )],
            })
            .collect();
        let (_cache, runtime) = setup_package(instances, package.clone()).await;
        let generation = prepare(&runtime).await;
        let plan = runtime.middleware_registry().resolve(&generation);
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
        let settings = gateway_core::settings::RequestSettings::new(Arc::new(snapshot));
        for size in [64 * 1024, 2 * 1024 * 1024] {
            let payload = Bytes::from(vec![b'a'; size]);
            for _ in 0..5 {
                benchmark_http_request(plan.as_ref(), &settings, payload.clone(), mode == "map")
                    .await;
            }
            let mut first_frames = Vec::new();
            let mut totals = Vec::new();
            for _ in 0..50 {
                let (first, total) = benchmark_http_request(
                    plan.as_ref(),
                    &settings,
                    payload.clone(),
                    mode == "map",
                )
                .await;
                first_frames.push(first.as_secs_f64() * 1000.0);
                totals.push(total.as_secs_f64() * 1000.0);
            }
            first_frames.sort_by(f64::total_cmp);
            totals.sort_by(f64::total_cmp);
            let started = std::time::Instant::now();
            for _ in 0..5 {
                futures::future::join_all((0..8).map(|_| {
                    benchmark_http_request(plan.as_ref(), &settings, payload.clone(), mode == "map")
                }))
                .await;
            }
            println!(
                "{}",
                serde_json::json!({
                    "case":label,"bytes":size,"samples":50,"concurrency":8,
                    "first_frame_p50_ms":first_frames[24],"first_frame_p95_ms":first_frames[47],
                    "complete_p50_ms":totals[24],"complete_p95_ms":totals[47],
                    "concurrent_mib_per_second":(size as f64 * 40.0 / 1048576.0) / started.elapsed().as_secs_f64(),
                })
            );
        }
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
    }
}

#[cfg(not(debug_assertions))]
async fn benchmark_http_request(
    plan: Option<&gateway_core::engine::middleware::FrozenMiddlewarePlan>,
    settings: &gateway_core::settings::RequestSettings,
    payload: Bytes,
    uppercase: bool,
) -> (Duration, Duration) {
    let length = payload.len();
    let frames = (0..length).step_by(64 * 1024).map(move |offset| {
        Ok::<_, std::io::Error>(Frame::data(
            payload.slice(offset..(offset + 64 * 1024).min(length)),
        ))
    });
    let mut request = ::http::Request::builder()
        .uri("/benchmark")
        .header("authorization", "Bearer fixture-secret")
        .body(
            StreamBody::new(futures::stream::iter(frames))
                .map_err(|error| Box::new(error) as _)
                .boxed_unsync(),
        )
        .unwrap();
    request.extensions_mut().insert(core::Settings {
        runtime: Some(settings.clone()),
        timeout: None,
    });
    let next = compose(Vec::new(), |request: core::Request| {
        Box::pin(async { Ok(core::Response::new(request.into_body())) })
    });
    let started = std::time::Instant::now();
    let response = match plan {
        Some(plan) => {
            plan.handle_http(
                core::Context {
                    plugin_instance_id: None,
                    request_id: "benchmark".into(),
                    call_id: "benchmark".into(),
                    parent_call_id: None,
                    extensions: Default::default(),
                    cancellation: CancellationToken::new(),
                    plan: Some(plan.clone()),
                },
                request,
                next,
            )
            .await
        }
        None => next.run(request).await,
    }
    .unwrap();
    let mut body = response.into_body();
    let mut first = None;
    let mut bytes = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Some(data) = frame.data_ref() {
            first.get_or_insert_with(|| started.elapsed());
            assert!(
                data.iter()
                    .all(|byte| *byte == if uppercase { b'A' } else { b'a' })
            );
            bytes += data.len();
        }
    }
    assert_eq!(bytes, length);
    (first.unwrap(), started.elapsed())
}
