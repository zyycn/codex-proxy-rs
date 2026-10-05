//! 验证插件 HTTP 子调用的上传容量、正文句柄与嵌套资源回收

use super::*;
use http_body::{Body, Frame};
use std::{
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};

struct Dispatcher {
    settings: Arc<Mutex<Vec<core::Settings>>>,
    calls: Arc<Mutex<Vec<core::Context>>>,
    entered: Arc<tokio::sync::Notify>,
    pending: bool,
    dropped: Arc<AtomicUsize>,
}

struct Pending(Arc<AtomicUsize>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct CheckedBody(core::Body, CancellationToken);
impl Body for CheckedBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        assert!(
            !self.1.is_cancelled(),
            "RPC End must not cancel a transferred response body"
        );
        Pin::new(&mut self.0).poll_frame(cx)
    }
}

impl core::Dispatcher for Dispatcher {
    fn dispatch(
        &self,
        context: core::Context,
        request: core::Request,
    ) -> BoxFuture<'static, Result<core::Response, MiddlewareError>> {
        self.calls.lock().unwrap().push(context.clone());
        if let Some(settings) = request.extensions().get::<core::Settings>().cloned() {
            self.settings.lock().unwrap().push(settings);
        }
        let entered = self.entered.clone();
        let dropped = self.dropped.clone();
        let pending = self.pending;
        Box::pin(async move {
            if pending {
                let _pending = Pending(dropped);
                entered.notify_one();
                return std::future::pending().await;
            }
            let cancellation = context.cancellation.clone();
            let next = compose(Vec::new(), move |request: core::Request| {
                Box::pin(async move {
                    let (parts, body) = request.into_parts();
                    let mut response =
                        core::Response::new(CheckedBody(body, cancellation).boxed_unsync());
                    *response.headers_mut() = parts.headers;
                    response.extensions_mut().insert(42_u32);
                    Ok(response)
                })
            });
            context
                .plan
                .clone()
                .unwrap()
                .handle_http(context, request, next)
                .await
        })
    }
}

async fn setup(
    mode: &str,
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
    let (cache, runtime) = setup_package(
        ["dispatch-a", "dispatch-b"]
            .into_iter()
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
            .collect(),
        package,
    )
    .await;
    let generation = prepare(&runtime).await;
    let plan = runtime.middleware_registry().resolve(&generation).unwrap();
    (cache, runtime, generation, plan)
}

fn root(plan: &gateway_core::engine::middleware::FrozenMiddlewarePlan) -> core::Context {
    core::Context {
        plugin_instance_id: None,
        request_id: "root-request".into(),
        call_id: "root-call".into(),
        parent_call_id: None,
        cancellation: CancellationToken::new(),
        extensions: Default::default(),
        plan: Some(plan.clone()),
    }
}

#[tokio::test]
async fn sequential_streamed_children_release_upload_capacity_in_one_call() {
    check_http_resources("sequential_uploads", 40).await;
}

#[tokio::test]
async fn upload_capacity_tracks_active_writers_and_closing_a_reader_unblocks_writes() {
    check_http_resources("upload_resources", 1).await;
}

async fn check_http_resources(mode: &str, children: usize) {
    let (_cache, runtime, generation, plan) = http_plugin(mode).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher: Arc<dyn core::Dispatcher> = Arc::new(Dispatcher {
        settings: Arc::default(),
        calls: calls.clone(),
        entered: Arc::default(),
        pending: false,
        dropped: Arc::default(),
    });
    runtime.bind_http(&dispatcher).unwrap();
    let response = plan
        .handle_http(
            root(&plan),
            core::Request::new(core::empty_body()),
            compose(Vec::new(), |request: core::Request| {
                Box::pin(async { Ok(core::Response::new(request.into_body())) })
            }),
        )
        .await
        .unwrap();
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(calls.lock().unwrap().len(), children);
    drop(dispatcher);
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn closing_body_preserves_response_parts_for_returning_a_replacement_body() {
    check_response_parts("close_response_body", "replacement").await;
}

#[tokio::test]
async fn returning_a_response_after_eof_preserves_parts_with_an_empty_body() {
    check_response_parts("read_response_body", "").await;
}

async fn check_response_parts(mode: &str, expected: &str) {
    let (_cache, runtime, generation, plan) = http_plugin(mode).await;
    let dropped = Arc::new(AtomicUsize::new(0));
    let lifetime = Pending(dropped.clone());
    let read_to_eof = mode == "read_response_body";
    let response = plan
        .handle_http(
            root(&plan),
            core::Request::new(core::empty_body()),
            compose(Vec::new(), move |_| {
                Box::pin(async move {
                    let body = StreamBody::new(futures::stream::once(async move {
                        let _lifetime = lifetime;
                        if !read_to_eof {
                            std::future::pending::<()>().await;
                        }
                        Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"original")))
                    }))
                    .map_err(|error| Box::new(error) as _)
                    .boxed_unsync();
                    let mut response = core::Response::new(body);
                    *response.status_mut() = ::http::StatusCode::ACCEPTED;
                    response
                        .headers_mut()
                        .insert("x-original", "kept".parse().unwrap());
                    response.extensions_mut().insert(42_u32);
                    Ok(response)
                })
            }),
        )
        .await
        .unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(response.status(), ::http::StatusCode::ACCEPTED);
    assert_eq!(response.headers()["x-original"], "kept");
    assert_eq!(response.extensions().get::<u32>(), Some(&42));
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        expected
    );
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn active_http_children_share_stream_resources_and_skip_all_ancestors() {
    for mode in ["dispatch", "dispatch_upload"] {
        let (_cache, runtime, generation, plan) = setup(mode).await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let dispatcher: Arc<dyn core::Dispatcher> = Arc::new(Dispatcher {
            settings: Arc::default(),
            calls: calls.clone(),
            entered: Arc::default(),
            pending: false,
            dropped: Arc::default(),
        });
        runtime.bind_http(&dispatcher).unwrap();
        let context = root(&plan);
        let cancellation = context.cancellation.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let expected = vec![b'a'; 2 * 1024 * 1024];
        let mut trailers = ::http::HeaderMap::new();
        trailers.append("x-trailer", "one".parse().unwrap());
        trailers.append("x-trailer", "two".parse().unwrap());
        let mut frames = vec![
            Frame::data(Bytes::from(expected.clone())),
            Frame::trailers(trailers),
        ]
        .into_iter();
        let body = StreamBody::new(futures::stream::poll_fn(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(frames.next().map(Ok::<_, std::io::Error>))
        }))
        .map_err(|error| Box::new(error) as _)
        .boxed_unsync();
        let request = ::http::Request::builder()
            .uri("/same/path?raw=%2f%FF")
            .header("x-duplicate", "one")
            .header("x-duplicate", "two")
            .body(body)
            .unwrap();
        let response = plan
            .handle_http(
                context,
                request,
                compose(Vec::new(), |_| {
                    Box::pin(async { panic!("dispatch replaces next") })
                }),
            )
            .await
            .unwrap();
        if mode == "dispatch" {
            assert_eq!(reads.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            response
                .headers()
                .get_all("x-dispatch-instance")
                .iter()
                .count(),
            2
        );
        assert_eq!(response.headers().get_all("x-duplicate").iter().count(), 2);
        assert_eq!(response.extensions().get::<u32>(), Some(&42));
        let collected = response.into_body().collect().await.unwrap();
        assert_eq!(
            collected
                .trailers()
                .unwrap()
                .get_all("x-trailer")
                .iter()
                .count(),
            2
        );
        assert_eq!(
            collected.to_bytes().as_ref(),
            if mode == "dispatch" {
                expected
            } else {
                expected.to_ascii_uppercase()
            }
        );
        assert!(!cancellation.is_cancelled());
        {
            let observed = calls.lock().unwrap();
            assert_eq!(observed.len(), 2);
            assert_eq!(observed[0].parent_call_id.as_deref(), Some("root-call"));
            assert_eq!(
                observed[1].parent_call_id.as_deref(),
                Some(observed[0].call_id.as_str())
            );
            assert_ne!(observed[0].call_id, observed[1].call_id);
            assert_eq!(observed[1].request_id, "root-request");
            assert!(observed[1].extensions.contains("dispatch-a"));
            assert!(observed[1].extensions.contains("dispatch-b"));
        }
        drop(dispatcher);
        drop(calls);
        drop(plan);
        drop(generation);
        runtime.shutdown().await;
    }
}

#[tokio::test]
async fn cancelling_a_parent_drops_its_active_http_child() {
    let (_cache, runtime, generation, plan) = setup("dispatch").await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let dispatcher: Arc<dyn core::Dispatcher> = Arc::new(Dispatcher {
        settings: Arc::default(),
        calls: Arc::default(),
        entered: entered.clone(),
        pending: true,
        dropped: dropped.clone(),
    });
    runtime.bind_http(&dispatcher).unwrap();
    let context = root(&plan);
    let cancellation = context.cancellation.clone();
    let request = core::Request::new(core::empty_body());
    let task = tokio::spawn(plan.handle_http(
        context,
        request,
        compose(Vec::new(), |_| {
            Box::pin(async { panic!("unexpected next") })
        }),
    ));
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    cancellation.cancel();
    assert!(task.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(3), async {
        while dropped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(dispatcher);
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}

#[tokio::test]
async fn active_http_children_inherit_settings_and_preserve_override_sources() {
    use gateway_core::{routing::RuntimeSnapshot, settings::RequestSettings};
    let (_cache, runtime, generation, plan) = setup("settings").await;
    let settings = Arc::new(Mutex::new(Vec::new()));
    let dispatcher: Arc<dyn core::Dispatcher> = Arc::new(Dispatcher {
        settings: settings.clone(),
        calls: Arc::default(),
        entered: Arc::default(),
        pending: false,
        dropped: Arc::default(),
    });
    runtime.bind_http(&dispatcher).unwrap();
    let baseline = RequestSettings::new(Arc::new(
        RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            gateway_core::settings::SettingsValues::new(
                1,
                10,
                "smart",
                Default::default(),
                None,
                None,
            ),
            vec![],
            vec![],
            vec![],
        )
        .unwrap(),
    ))
    .with_http_timeout(Some(60_000));
    let mut request = core::Request::new(core::empty_body());
    request.extensions_mut().insert(core::Settings {
        timeout: Some(Duration::from_secs(60)),
        runtime: Some(baseline.clone()),
    });
    plan.handle_http(
        root(&plan),
        request,
        compose(Vec::new(), |_| {
            Box::pin(async { panic!("explicit dispatch replaces next") })
        }),
    )
    .await
    .unwrap()
    .into_body()
    .collect()
    .await
    .unwrap();
    {
        let observed = settings.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].timeout, Some(Duration::from_secs(90)));
        assert_eq!(observed[1].timeout, None);
        let effective = observed[1].runtime.as_ref().unwrap();
        let final_settings = serde_json::to_value(effective.values()).unwrap();
        assert_eq!(
            final_settings["responses_max_decompressed_body_bytes"],
            8192
        );
        assert_eq!(final_settings["request_interval_ms"], 0);
        assert!(final_settings["min_codex_cli_version"].is_null());
        let sources = effective.inspect();
        assert_eq!(
            sources["overrides"]["min_codex_cli_version"]["instance_id"],
            "dispatch-b"
        );
        assert_eq!(
            sources["overrides"]["request_interval_ms"]["instance_id"],
            "dispatch-a"
        );
        assert!(
            sources["overrides"]["min_codex_cli_version"]["order"]
                .as_u64()
                .unwrap()
                > sources["overrides"]["request_interval_ms"]["order"]
                    .as_u64()
                    .unwrap()
        );
        assert_eq!(sources["http_timeout"]["input_ms"], 60_000);
        assert_eq!(
            sources["http_timeout"]["change"]["instance_id"],
            "dispatch-b"
        );
        assert!(sources["http_timeout"]["change"]["value"].is_null());
        assert_eq!(
            sources["host"]["responses_max_decompressed_body_bytes"],
            64 * 1024 * 1024
        );
        assert!(
            baseline.inspect()["overrides"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
    drop(dispatcher);
    drop(plan);
    drop(generation);
    runtime.shutdown().await;
}
