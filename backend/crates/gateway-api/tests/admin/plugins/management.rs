//! 插件管理页面模型调用的认证、执行与响应交付测试

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::future::BoxFuture;
use gateway_core::{
    engine::{
        execution::{
            AuthenticatedClient, ClientAuthenticationError, ExecutionService,
            PreparedRootExecution, StartExecution, StartProviderExecution, StartedExecution,
        },
        middleware::{
            FrozenMiddlewarePlan, MiddlewareBody, MiddlewareContext, MiddlewareError,
            MiddlewareFrame, MiddlewareFraming, MiddlewareHeader, MiddlewareNext, MiddlewarePlan,
            MiddlewareRequest, MiddlewareResponse,
        },
    },
    error::GatewayError,
    lifecycle::CancellationToken,
    policy::ClientApiKeyId,
    routing::PublicModelId,
    runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tower::ServiceExt as _;

use super::super::AdminTestFixture;

#[derive(Debug, Clone, Copy)]
enum ModelReply {
    Complete,
    PendingResponse,
    PendingBody,
}

struct ModelExecution {
    reply: ModelReply,
    cancellation: Mutex<Option<CancellationToken>>,
    entered: Arc<tokio::sync::Notify>,
    closed: Arc<AtomicBool>,
}

impl ModelExecution {
    fn new(reply: ModelReply) -> Arc<Self> {
        Arc::new(Self {
            reply,
            cancellation: Mutex::new(None),
            entered: Arc::default(),
            closed: Arc::default(),
        })
    }
}

impl ExecutionService for ModelExecution {
    fn authenticate(&self, _: &str) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        panic!("管理模型桥不能使用管理 Cookie 或 Bearer 作为 Client Key")
    }
    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        vec![]
    }
    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        true
    }
    fn prepare_plugin_execution(
        &self,
        key: &ClientApiKeyId,
    ) -> BoxFuture<'_, Result<PreparedRootExecution, GatewayError>> {
        assert_eq!(key.as_str(), "client-key");
        Box::pin(async move {
            let prepared = self
                .prepare_execution(crate::openai::authenticated_client("fixture-client-key"))
                .await?;
            *self.cancellation.lock().unwrap() = Some(prepared.cancellation());
            Ok(prepared)
        })
    }
    fn middleware_plan(&self, _: &PreparedRootExecution) -> Option<FrozenMiddlewarePlan> {
        Some(FrozenMiddlewarePlan::new(
            Arc::new(ModelMiddleware {
                reply: self.reply,
                entered: self.entered.clone(),
                closed: self.closed.clone(),
            }),
            ExtensionSetReference::new(
                ExtensionSetId::new("model-bridge-test".into()).unwrap(),
                Arc::new(ModelLease),
            ),
        ))
    }
    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        panic!("测试中间件直接返回响应")
    }
    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        panic!("Responses 不走 Provider 端点")
    }
}

struct ModelLease;
impl ExtensionSetLease for ModelLease {
    fn is_ready(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct ModelMiddleware {
    reply: ModelReply,
    entered: Arc<tokio::sync::Notify>,
    closed: Arc<AtomicBool>,
}

impl MiddlewarePlan for ModelMiddleware {
    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        _: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        for (name, value) in [
            ("cookie", "cpr_session=management-session".to_owned()),
            ("authorization", "Bearer admin-fixture".to_owned()),
            ("x-api-key", format!("admin-{}", "a".repeat(64))),
        ] {
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|header| { header.name() == name && header.value() == value.as_bytes() })
            );
        }
        assert!(
            request
                .headers()
                .iter()
                .any(|header| header.name() == "content-type")
        );
        let reply = self.reply;
        let closed = self.closed.clone();
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.notify_one();
            if matches!(reply, ModelReply::PendingResponse) {
                return futures::future::pending().await;
            }
            let (bytes, framing, terminal) = if matches!(reply, ModelReply::PendingBody) {
                (
                    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bridge\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                    MiddlewareFraming::SseEvent,
                    false,
                )
            } else {
                (
                    "{\"id\":\"resp_bridge\",\"object\":\"response\",\"status\":\"completed\",\"output\":[]}",
                    MiddlewareFraming::JsonDocument,
                    true,
                )
            };
            Ok(MiddlewareResponse::new(
                "openai".into(),
                200,
                vec![
                    MiddlewareHeader::new("set-cookie", "fixture=value".into()),
                    MiddlewareHeader::new("www-authenticate", "Bearer".into()),
                    MiddlewareHeader::new("x-plugin-response", "complete".into()),
                ],
                Box::new(ModelBody {
                    frame: Some(MiddlewareFrame::new(bytes.into(), framing, terminal)),
                    pending: !terminal,
                    closed,
                }),
            ))
        })
    }
}

struct ModelBody {
    frame: Option<MiddlewareFrame>,
    pending: bool,
    closed: Arc<AtomicBool>,
}

impl MiddlewareBody for ModelBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            if let Some(frame) = self.frame.take() {
                return Ok(Some(frame));
            }
            if self.pending {
                return futures::future::pending().await;
            }
            Ok(None)
        })
    }
    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.closed.store(true, Ordering::SeqCst);
        })
    }
}

fn model_request(stream: bool) -> Request<Body> {
    Request::post(format!("/api/admin/plugins/extensions/plugin-instance-diagnostic/{}/7/models/responses?clientKeyId=client-key", "a".repeat(64)))
        .header("cookie", "cpr_session=management-session")
        .header("authorization", "Bearer admin-fixture")
        .header("x-api-key", format!("admin-{}", "a".repeat(64)))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({"model":"model-a", "input":"hello", "stream":stream}).to_string())).unwrap()
}

#[tokio::test]
async fn management_calls_and_callbacks_preserve_headers_and_override_defaults() {
    let fixture = published_fixture().await;
    let target = format!("plugin-instance-diagnostic/{}/7", "a".repeat(64));
    for uri in [
        format!("/api/admin/plugins/extensions/{target}/api/echo"),
        format!("/plugins/callbacks/{target}/oauth?state=nonce"),
    ] {
        let response = crate::openai::api_router_with_admin_and_execution(
            fixture.services.clone(),
            ModelExecution::new(ModelReply::Complete),
        )
        .oneshot(
            Request::get(uri)
                .header("cookie", "cpr_session=management-session")
                .header("authorization", "Bearer management-fixture")
                .header(
                    "x-plugin-binary",
                    axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
                )
                .header("set-cookie", "first=1")
                .header("set-cookie", "second=2")
                .header("content-security-policy", "default-src 'self'")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["cookie"],
            "cpr_session=management-session"
        );
        assert_eq!(
            response.headers()["authorization"],
            "Bearer management-fixture"
        );
        assert_eq!(response.headers()["x-plugin-binary"].as_bytes(), &[0xff]);
        assert_eq!(
            response
                .headers()
                .get_all("set-cookie")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["first=1", "second=2"]
        );
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'self'"
        );
    }
}

#[tokio::test]
async fn model_bridge_preserves_complete_request_and_response_headers() {
    let fixture = published_fixture().await;
    let execution = ModelExecution::new(ModelReply::Complete);
    let response = crate::openai::api_router_with_admin_and_execution(fixture.services, execution)
        .oneshot(model_request(false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["set-cookie"], "fixture=value");
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    assert_eq!(response.headers()["x-plugin-response"], "complete");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"],
        "resp_bridge"
    );
}

#[tokio::test(start_paused = true)]
async fn model_bridge_revokes_while_waiting_for_the_first_response() {
    let fixture = published_fixture().await;
    let execution = ModelExecution::new(ModelReply::PendingResponse);
    let router =
        crate::openai::api_router_with_admin_and_execution(fixture.services, execution.clone());
    let request = tokio::spawn(router.oneshot(model_request(false)));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        execution.entered.notified(),
    )
    .await
    .expect("请求已进入模型执行链");
    *fixture.plugin_ports.instances.lock().unwrap() = Some(vec![]);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert_eq!(
        request.await.unwrap().unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        execution
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
}

#[tokio::test(start_paused = true)]
async fn model_bridge_revokes_and_closes_a_pending_stream() {
    let fixture = published_fixture().await;
    let execution = ModelExecution::new(ModelReply::PendingBody);
    let response =
        crate::openai::api_router_with_admin_and_execution(fixture.services, execution.clone())
            .oneshot(model_request(true))
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = tokio::spawn(axum::body::to_bytes(response.into_body(), 4096));
    *fixture.plugin_ports.instances.lock().unwrap() = Some(vec![]);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let body = body.await.unwrap().unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(body.contains("response.failed"));
    assert!(!body.contains("response.completed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
    assert!(
        execution
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    assert!(execution.closed.load(Ordering::SeqCst));
}

pub(super) fn resource_fixture(
    path: &str,
) -> gateway_admin::model::plugins::management::PluginManagementResponse {
    gateway_admin::model::plugins::management::PluginManagementResponse {
        headers: Vec::new(),
        status: if path == "missing" { 404 } else { 200 },
        content_type: if path.ends_with(".css") {
            "text/css"
        } else {
            "text/html"
        }
        .into(),
        body: std::sync::Arc::from(path.as_bytes()),
    }
}

async fn published_fixture() -> AdminTestFixture {
    use gateway_core::{
        routing::{ConfigRevision, RuntimeSnapshot},
        runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
    };
    use std::sync::Arc;
    struct Lease;
    impl ExtensionSetLease for Lease {
        fn is_ready(&self) -> bool {
            true
        }
    }
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("management-session");
    fixture
        .auth
        .set_api_key(&format!("admin-{}", "a".repeat(64)));
    fixture.published_snapshot.publish(
        RuntimeSnapshot::new(
            ConfigRevision::new(7).unwrap(),
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
        .unwrap()
        .with_extensions(Some(ExtensionSetReference::new(
            ExtensionSetId::new("management-test".into()).unwrap(),
            Arc::new(Lease),
        ))),
    );
    fixture
}

async fn resource_request(
    fixture: &AdminTestFixture,
    method: &str,
    uri: &str,
    session: bool,
    validators: &[&str],
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "management-cache-test");
    if session {
        request = request.header("cookie", "cpr_session=management-session");
    }
    for validator in validators {
        request = request.header("if-none-match", *validator);
    }
    crate::openai::api_router_with_admin_and_execution(
        fixture.services.clone(),
        ModelExecution::new(ModelReply::Complete),
    )
    .oneshot(request.body(Body::empty()).unwrap())
    .await
    .unwrap()
}

#[tokio::test]
async fn static_resources_revalidate_privately_after_authorization() {
    use axum::body::to_bytes;
    use gateway_admin::ports::plugins::PluginStore as _;

    let fixture = published_fixture().await;
    let prefix = format!(
        "/api/admin/plugins/extensions/plugin-instance-diagnostic/{}/7",
        "a".repeat(64)
    );
    let uri = format!("{prefix}/resources/ui/index.html");
    let first = resource_request(&fixture, "GET", &uri, true, &[]).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["cache-control"], "private, no-cache");
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    let csp = first.headers()["content-security-policy"].clone();
    assert_eq!(
        to_bytes(first.into_body(), 1024).await.unwrap(),
        "ui/index.html"
    );

    for validators in [
        vec![etag.as_str()],
        vec!["*"],
        vec!["\"other,tag\"", etag.as_str()],
        vec!["\"other\"", &format!("W/{etag}")],
    ] {
        for method in ["GET", "HEAD"] {
            let response = resource_request(&fixture, method, &uri, true, &validators).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "{validators:?}"
            );
            assert_eq!(response.headers()["etag"], etag);
            assert_eq!(response.headers()["cache-control"], "private, no-cache");
            assert_eq!(response.headers()["content-security-policy"], csp);
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(
                to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
    for validators in [vec!["\"other\""], vec!["invalid"]] {
        assert_eq!(
            resource_request(&fixture, "GET", &uri, true, &validators)
                .await
                .status(),
            StatusCode::OK
        );
    }
    let changed = resource_request(
        &fixture,
        "GET",
        &format!("{prefix}/resources/ui/other.css"),
        true,
        &[&etag],
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()["etag"], etag);
    let unauthenticated = resource_request(&fixture, "GET", &uri, false, &[&etag]).await;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthenticated.headers()["cache-control"], "no-store");

    let original = fixture
        .plugin_ports
        .load_instances()
        .await
        .unwrap()
        .instances;
    for change in ["disable", "untrust", "revision", "version"] {
        let mut instances = original.clone();
        match change {
            "disable" => instances[0].enabled = false,
            "untrust" => instances[0].trusted_process = false,
            "revision" => instances[0].revision = gateway_admin::model::Revision::new(8).unwrap(),
            _ => instances[0].artifact_sha256 = "b".repeat(64),
        }
        *fixture.plugin_ports.instances.lock().unwrap() = Some(instances);
        let response = resource_request(&fixture, "GET", &uri, true, &[&etag]).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{change}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("etag"));
    }
}

#[tokio::test]
async fn public_resources_cache_but_dynamic_calls_callbacks_and_errors_do_not() {
    let fixture = published_fixture().await;
    let target = format!("plugin-instance-diagnostic/{}/7", "a".repeat(64));
    let public = format!("/plugins/resources/{target}/public/login.css");
    let first = resource_request(&fixture, "GET", &public, false, &[]).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()["etag"].to_str().unwrap();
    assert_eq!(
        resource_request(&fixture, "GET", &public, false, &[etag])
            .await
            .status(),
        StatusCode::NOT_MODIFIED
    );
    for (uri, status) in [
        (
            format!("/plugins/resources/{target}/ui/index.html"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/api/admin/plugins/extensions/{target}/resources/missing"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/api/admin/plugins/extensions/{target}/api/echo"),
            StatusCode::OK,
        ),
        (
            format!("/plugins/callbacks/{target}/oauth?state=nonce"),
            StatusCode::OK,
        ),
    ] {
        let response = resource_request(&fixture, "GET", &uri, true, &["*"]).await;
        assert_eq!(response.status(), status, "{uri}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("etag"));
    }
    *fixture.plugin_ports.instances.lock().unwrap() = Some(vec![]);
    let response = resource_request(&fixture, "GET", &public, false, &[etag]).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn model_responses_requires_the_exact_authorized_target_before_execution() {
    let fixture = published_fixture().await;
    let target = format!("plugin-instance-diagnostic/{}/7", "a".repeat(64));
    let request = |target: &str, query: &str| {
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/admin/plugins/extensions/{target}/models/responses?{query}"
            ))
            .header("cookie", "cpr_session=management-session")
            .header("authorization", "Bearer admin-fixture")
            .header("x-api-key", format!("admin-{}", "a".repeat(64)))
            .header("content-type", "application/json")
            .header("x-request-id", "management-model-request")
            .body(Body::from(
                serde_json::json!({"model":"model-a","input":"hello","stream":false}).to_string(),
            ))
            .unwrap()
    };

    let response = crate::openai::api_router_with_admin_and_execution(
        fixture.services.clone(),
        ModelExecution::new(ModelReply::Complete),
    )
    .oneshot(request(&target, "clientKeyId=client-key"))
    .await
    .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "精确目标通过授权后应执行并返回结果"
    );

    for changed in [
        format!("plugin-instance-diagnostic/{}/8", "a".repeat(64)),
        format!("plugin-instance-diagnostic/{}/7", "b".repeat(64)),
    ] {
        let response = crate::openai::api_router_with_admin_and_execution(
            fixture.services.clone(),
            ModelExecution::new(ModelReply::Complete),
        )
        .oneshot(request(&changed, "clientKeyId=client-key"))
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    let response = crate::openai::api_router_with_admin_and_execution(
        fixture.services.clone(),
        ModelExecution::new(ModelReply::Complete),
    )
    .oneshot(request(
        &target,
        "clientKeyId=client-key&accountId=forbidden",
    ))
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn protected_plugin_routes_require_admin_before_reading_bodies_but_public_resources_do_not() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("management-session");
    let prefix = "/api/admin/plugins/extensions/instance/sha256/1";
    for (method, uri) in [
        ("GET", "/api/admin/plugins/extensions".to_owned()),
        ("GET", format!("{prefix}/resources/ui/index.html")),
        ("POST", format!("{prefix}/api/echo")),
        ("POST", format!("{prefix}/callback-tickets")),
        (
            "POST",
            format!("{prefix}/models/responses?clientKeyId=client-key"),
        ),
    ] {
        let response = crate::openai::api_router_with_admin_and_execution(
            fixture.services.clone(),
            ModelExecution::new(ModelReply::Complete),
        )
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("x-request-id", "management-test")
                .body(Body::from_stream(futures::stream::poll_fn(
                    |_| -> std::task::Poll<Option<Result<bytes::Bytes, std::io::Error>>> {
                        panic!("未认证请求不能读取正文")
                    },
                )))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let response = crate::openai::api_router_with_admin_and_execution(
        fixture.services.clone(),
        ModelExecution::new(ModelReply::Complete),
    )
    .oneshot(
        Request::builder()
            .uri("/plugins/resources/instance/sha256/1/ui/index.html")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "公开资源仍要求可用的发布视图，不能绕过实例授权"
    );
    let response = crate::openai::api_router_with_admin_and_execution(
        fixture.services.clone(),
        ModelExecution::new(ModelReply::Complete),
    )
    .oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("{prefix}/api/echo"))
            .header("x-request-id", "management-limit")
            .header("cookie", "cpr_session=management-session")
            .header("content-type", "application/octet-stream")
            .body(Body::from(vec![0_u8; 1024 * 1024 + 1]))
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let response = crate::openai::api_router_with_admin_and_execution(
        fixture.services.clone(),
        ModelExecution::new(ModelReply::Complete),
    )
    .oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("{prefix}/models/responses?clientKeyId=client-key"))
            .header("x-request-id", "management-model-limit")
            .header("cookie", "cpr_session=management-session")
            .header("content-type", "application/json")
            .body(Body::from(vec![0_u8; 8 * 1024 * 1024 + 1]))
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn public_callback_rejects_head_missing_duplicate_state_and_body_before_dispatch() {
    let fixture = AdminTestFixture::new().await;
    let prefix = "/plugins/callbacks/instance/sha256/1/oauth";
    for (method, query, body, expected) in [
        ("HEAD", "state=one", "", StatusCode::METHOD_NOT_ALLOWED),
        ("GET", "code=one", "", StatusCode::BAD_REQUEST),
        ("GET", "state=", "", StatusCode::BAD_REQUEST),
        ("GET", "state=one&state=two", "", StatusCode::BAD_REQUEST),
        ("GET", "state=one&%73tate=two", "", StatusCode::BAD_REQUEST),
        ("GET", "state=one", "body", StatusCode::BAD_REQUEST),
        // 合法 GET 没有管理 Cookie 仍进入服务；当前测试未发布视图，返回 503 而不是 401
        ("GET", "state=one", "", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let response = crate::openai::api_router_with_admin_and_execution(
            fixture.services.clone(),
            ModelExecution::new(ModelReply::Complete),
        )
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("{prefix}?{query}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), expected, "{method} {query}");
    }
}
