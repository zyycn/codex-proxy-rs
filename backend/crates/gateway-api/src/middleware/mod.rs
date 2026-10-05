//! 路由外层只适配 HTTP 值和正文，不解释插件协议或业务策略

pub(crate) mod headers;
pub(crate) mod websocket;

use std::{
    convert::Infallible,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{HeaderName, HeaderValue, Request, StatusCode},
    response::{IntoResponse as _, Response},
};
use gateway_core::{
    engine::middleware::FrozenMiddlewarePlan,
    lifecycle::CancellationToken,
    middleware::{
        compose,
        http::{self as contract, Settings},
    },
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use http_body_util::BodyExt as _;
use tokio::time::Instant;
use tower::ServiceExt as _;

tokio::task_local! { static CURRENT: contract::Context; }
tokio::task_local! { static SETTINGS: Option<gateway_core::settings::RequestSettings>; }

pub(crate) fn current_settings() -> Option<gateway_core::settings::RequestSettings> {
    SETTINGS.try_with(Clone::clone).ok().flatten()
}

pub(crate) fn current() -> Option<contract::Context> {
    CURRENT.try_with(Clone::clone).ok()
}

pub(crate) async fn scope<T>(
    context: Option<contract::Context>,
    future: impl std::future::Future<Output = T>,
) -> T {
    match context {
        Some(context) => CURRENT.scope(context, future).await,
        None => future.await,
    }
}

pub(crate) type SettingsSource =
    Arc<dyn Fn() -> Option<gateway_core::settings::RequestSettings> + Send + Sync>;

pub(crate) struct RouterDispatcher {
    pub(crate) router: Router,
    pub(crate) settings: SettingsSource,
}

impl contract::Dispatcher for RouterDispatcher {
    fn request_settings(&self) -> Option<gateway_core::settings::RequestSettings> {
        (self.settings)()
    }

    fn dispatch(
        &self,
        context: contract::Context,
        mut request: contract::Request,
    ) -> futures::future::BoxFuture<
        'static,
        Result<contract::Response, gateway_core::engine::middleware::MiddlewareError>,
    > {
        let router = self.router.clone();
        let cancellation = context.cancellation.clone();
        if let Some(instance_id) = &context.plugin_instance_id {
            request
                .extensions_mut()
                .insert(gateway_admin::model::auth::AdminRequestContext {
                    principal: gateway_admin::model::auth::AdminPrincipal::Plugin {
                        instance_id: instance_id.clone(),
                    },
                    request_id: format!(
                        "plugin:{instance_id}:call:{}:request:{}",
                        context.call_id, context.request_id
                    ),
                });
        }
        request.extensions_mut().insert(context);
        Box::pin(async move {
            let response = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(gateway_core::engine::middleware::MiddlewareError::Gateway(gateway_core::error::GatewayError::new(gateway_core::error::GatewayErrorKind::Cancelled, "HTTP child cancelled"))),
                result = router.oneshot(request.map(Body::new)) => match result { Ok(response) => response, Err(never) => match never {} },
            };
            Ok(response.map(|body| body.map_err(|error| Box::new(error) as _).boxed_unsync()))
        })
    }
}

pub(crate) type PlanSource = Arc<
    dyn Fn(Option<&gateway_core::routing::RuntimeSnapshot>) -> Option<FrozenMiddlewarePlan>
        + Send
        + Sync,
>;

pub(crate) fn wrap(
    router: Router,
    source: Option<PlanSource>,
    timeout: Option<Duration>,
    request_id_header: HeaderName,
    settings: SettingsSource,
) -> Router {
    // Axum 的 route layer 在匹配后运行；外层 fallback service 才能让改写后的 URI 重新匹配内层路由
    Router::new().fallback_service(tower::service_fn(move |mut request: Request<Body>| {
        let router = router.clone();
        let inherited = request.extensions().get::<contract::Context>().is_some();
        let configuration = request.extensions_mut().get_or_insert(Settings {
            timeout,
            runtime: None,
        });
        if configuration.runtime.is_none() && (source.is_some() || inherited) {
            configuration.runtime = settings();
        }
        let snapshot = configuration
            .runtime
            .as_ref()
            .map(|settings| settings.snapshot());
        let plan = request
            .extensions()
            .get::<contract::Context>()
            .and_then(|context| context.plan.clone())
            .or_else(|| {
                source
                    .as_ref()
                    .and_then(|source| source(snapshot.as_deref()))
            });
        let request_id_header = request_id_header.clone();
        async move { Ok::<_, Infallible>(handle(router, request, plan, request_id_header).await) }
    }))
}

async fn handle(
    router: Router,
    mut request: Request<Body>,
    plan: Option<FrozenMiddlewarePlan>,
    request_id_header: HeaderName,
) -> Response {
    let started = Instant::now();
    let inherited = request.extensions_mut().remove::<contract::Context>();
    if let Some(plan) = plan.as_ref().filter(|plan| plan.has_websocket()) {
        request.extensions_mut().insert(plan.clone());
    }
    if inherited.is_none()
        && !plan
            .as_ref()
            .is_some_and(|plan| plan.has_http() || plan.has_service())
    {
        let settings = request
            .extensions()
            .get::<Settings>()
            .and_then(|settings| settings.runtime.clone());
        return SETTINGS
            .scope(settings, dispatch(router, request, started))
            .await;
    }
    let context = inherited.unwrap_or_else(|| {
        let request_id = request
            .headers()
            .get(&request_id_header)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        contract::Context {
            plugin_instance_id: None,
            call_id: request_id.clone(),
            request_id,
            parent_call_id: None,
            extensions: Default::default(),
            cancellation: CancellationToken::new(),
            plan: plan.clone(),
        }
    });
    let context = contract::Context {
        plan: plan.clone(),
        ..context
    };
    if let Ok(value) = HeaderValue::from_str(&context.request_id) {
        request.headers_mut().insert(request_id_header, value);
    }
    let guard = Arc::new(CallLifetime {
        cancellation: context.cancellation.clone(),
        _plan: plan.clone(),
    });
    request.extensions_mut().insert(guard.clone());
    request.extensions_mut().insert(context.clone());
    let service_context = gateway_admin::service::Origin {
        request_id: context.request_id.clone(),
        call_id: context.call_id.clone(),
        cancellation: context.cancellation.clone(),
        extensions: context.extensions.clone(),
        plan: gateway_admin::service::Plan::Frozen(plan.clone()),
    };
    request
        .extensions_mut()
        .insert(Arc::new(websocket::Upgrade {
            context: context.clone(),
        }) as Arc<dyn contract::upgrade::WebSocketUpgrade>);
    let request = request.map(|body| body.map_err(|error| Box::new(error) as _).boxed_unsync());
    let terminal_context = context.clone();
    let next = compose(Vec::new(), move |request: contract::Request| {
        Box::pin(async move {
            let settings = request
                .extensions()
                .get::<Settings>()
                .and_then(|settings| settings.runtime.clone());
            let response = CURRENT
                .scope(
                    terminal_context,
                    SETTINGS.scope(
                        settings,
                        gateway_admin::service::scope(
                            service_context,
                            dispatch(router, request.map(Body::new), started),
                        ),
                    ),
                )
                .await;
            Ok(response.map(|body| body.map_err(|error| Box::new(error) as _).boxed_unsync()))
        })
    });
    let result = match plan {
        Some(plan) => plan.handle_http(context, request, next).await,
        None => next.run(request).await,
    };
    match result {
        Ok(mut response) => {
            if let Some(upgraded) = response
                .extensions_mut()
                .remove::<contract::upgrade::Upgraded>()
            {
                if response.status() != upgraded.status {
                    return StatusCode::BAD_GATEWAY.into_response();
                }
                let Some(task) = upgraded.take_task() else {
                    return StatusCode::BAD_GATEWAY.into_response();
                };
                let lifetime = guard.clone();
                tokio::spawn(async move {
                    let cancellation = lifetime.cancellation.clone();
                    let _lifetime = lifetime;
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {},
                        _ = task => {},
                    }
                });
            }
            response.map(|body| Body::new(ResponseBody::new(body, guard)))
        }
        Err(error) => {
            let status = if error.is_rejected() {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_GATEWAY
            };
            (status, axum::Json(serde_json::json!({"error": {"message": "HTTP middleware failed", "type": "middleware_error"}}))).into_response()
        }
    }
}

async fn dispatch(router: Router, request: Request<Body>, started: Instant) -> Response {
    let timeout = request
        .extensions()
        .get::<Settings>()
        .and_then(|settings| settings.timeout);
    let dispatch = router.oneshot(request);
    let result = match timeout {
        Some(timeout) => {
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return StatusCode::REQUEST_TIMEOUT.into_response();
            };
            match tokio::time::timeout(remaining, dispatch).await {
                Ok(result) => result,
                Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
            }
        }
        None => dispatch.await,
    };
    match result {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

pub(crate) struct CallLifetime {
    cancellation: CancellationToken,
    _plan: Option<FrozenMiddlewarePlan>,
}

impl Drop for CallLifetime {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct ResponseBody {
    body: contract::Body,
    _lifetime: Arc<CallLifetime>,
    cancellation: futures::future::BoxFuture<'static, ()>,
    finished: bool,
}

impl ResponseBody {
    fn new(body: contract::Body, lifetime: Arc<CallLifetime>) -> Self {
        let cancellation = lifetime.cancellation.clone();
        Self {
            body,
            _lifetime: lifetime,
            cancellation: Box::pin(async move { cancellation.cancelled().await }),
            finished: false,
        }
    }
}

impl HttpBody for ResponseBody {
    type Data = bytes::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if self.cancellation.as_mut().poll(context).is_ready() {
            self.finished = true;
            self.body = contract::empty_body();
            return Poll::Ready(Some(Err(Box::new(gateway_core::error::GatewayError::new(
                gateway_core::error::GatewayErrorKind::Cancelled,
                "HTTP body cancelled",
            )))));
        }
        let frame = Pin::new(&mut self.body).poll_frame(context);
        if matches!(frame, Poll::Ready(None)) {
            self.finished = true;
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.finished || self.body.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}
