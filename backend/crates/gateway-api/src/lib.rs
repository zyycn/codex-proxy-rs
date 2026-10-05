//! 客户端与管理端 HTTP 协议 adapter
//!
//! 本 crate 只负责请求解码、Core/Admin 调用和 HTTP/WS/SSE delivery

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, Method, Request};
use axum::routing::get;
use gateway_admin::AdminServices;
use gateway_core::engine::execution::ExecutionService;
use gateway_core::health::{HealthProbe, WorkerHealthSource};
use gateway_core::lifecycle::ConnectionLifecycle;
use serde::Deserialize;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use url::Url;

const WEB_DIST_ENV: &str = "CPR_WEB_DIST_DIR";

use crate::health::HealthStatus;
use crate::openai::service::OpenAiService;

pub mod admin;
pub mod auth;
mod health;
mod key_usage;
mod middleware;
pub mod openai;
mod provider;
mod session_cookie;
mod time;

pub use time::{RequestBucketView, TimePresenter};

/// API-owned HTTP 与静态资源配置
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ApiConfig {
    pub asset_directory: PathBuf,
    pub cors_allowed_origins: Vec<String>,
    pub request_timeout_seconds: Option<u64>,
    pub request_id_header: String,
}

impl ApiConfig {
    /// 解析静态资源相对路径并校验全部 HTTP 配置
    ///
    /// # Errors
    ///
    /// 路径为空、origin/header 非法或 timeout 为零时返回脱敏错误
    pub fn resolve_and_validate(&mut self, source_dir: &Path) -> Result<(), ApiConfigError> {
        match std::env::var(WEB_DIST_ENV) {
            Ok(value) if value.trim().is_empty() => {
                return Err(ApiConfigError::InvalidAssetDirectory);
            }
            Ok(value) => self.asset_directory = PathBuf::from(value),
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ApiConfigError::InvalidAssetDirectory);
            }
        }
        self.validate()?;
        if self.asset_directory.is_relative() {
            self.asset_directory = source_dir.join(&self.asset_directory);
        }
        Ok(())
    }

    fn validate(&mut self) -> Result<(), ApiConfigError> {
        if self.asset_directory.as_os_str().is_empty() {
            return Err(ApiConfigError::InvalidAssetDirectory);
        }
        if self.request_timeout_seconds == Some(0) {
            return Err(ApiConfigError::InvalidRequestTimeout);
        }
        let header = HeaderName::from_str(&self.request_id_header)
            .map_err(|_| ApiConfigError::InvalidRequestIdHeader)?;
        self.request_id_header = header.as_str().to_owned();

        let mut origins = Vec::with_capacity(self.cors_allowed_origins.len());
        let mut unique = BTreeSet::new();
        for origin in &self.cors_allowed_origins {
            let origin = validate_origin(origin)?;
            if !unique.insert(origin.clone()) {
                return Err(ApiConfigError::DuplicateCorsOrigin);
            }
            origins.push(origin);
        }
        self.cors_allowed_origins = origins;
        Ok(())
    }
}

/// API 配置非法的稳定分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiConfigError {
    #[error("API asset directory is invalid")]
    InvalidAssetDirectory,
    #[error("API CORS origin is invalid")]
    InvalidCorsOrigin,
    #[error("API CORS origin is duplicated")]
    DuplicateCorsOrigin,
    #[error("API request timeout is invalid")]
    InvalidRequestTimeout,
    #[error("API request ID header is invalid")]
    InvalidRequestIdHeader,
}

/// 完成组装的唯一 API router
pub struct ApiBundle {
    router: Router,
    settings: middleware::SettingsSource,
    middleware: Option<middleware::PlanSource>,
    timeout: Option<Duration>,
    request_id_header: HeaderName,
}

impl ApiBundle {
    /// 组合根从请求已冻结的快照解析计划；响应流持有同一代次
    #[must_use]
    pub fn with_middleware(
        mut self,
        source: impl Fn(
            Option<&gateway_core::routing::RuntimeSnapshot>,
        ) -> Option<gateway_core::engine::middleware::FrozenMiddlewarePlan>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.middleware = Some(Arc::new(source));
        self
    }

    /// 内部调用使用同一总路由；组合根持有强引用，Runtime 只保存 Weak
    pub fn dispatcher(&self) -> Arc<dyn gateway_core::middleware::http::Dispatcher> {
        Arc::new(middleware::RouterDispatcher {
            router: middleware::wrap(
                self.router.clone(),
                self.middleware.clone(),
                self.timeout,
                self.request_id_header.clone(),
                self.settings.clone(),
            ),
            settings: self.settings.clone(),
        })
    }

    pub fn router(self) -> Router {
        middleware::wrap(
            self.router,
            self.middleware,
            self.timeout,
            self.request_id_header,
            self.settings,
        )
    }
}

/// 组装客户端、管理端、健康检查和静态资源路由
pub fn initialize(
    mut config: ApiConfig,
    execution: Arc<dyn ExecutionService>,
    admin: AdminServices,
    probes: Vec<Arc<dyn HealthProbe>>,
    worker_health: Arc<dyn WorkerHealthSource>,
    lifecycle: Arc<dyn ConnectionLifecycle>,
) -> Result<ApiBundle, ApiError> {
    // 配置加载已解析环境变量与相对路径，初始化只校验，避免覆盖最终目录
    config.validate().map_err(ApiError::Config)?;
    let request_id_header = HeaderName::from_str(&config.request_id_header)
        .map_err(|_| ApiError::Config(ApiConfigError::InvalidRequestIdHeader))?;
    let settings: middleware::SettingsSource = {
        let execution = execution.clone();
        let timeout_ms = config
            .request_timeout_seconds
            .map(|seconds| seconds.saturating_mul(1000));
        Arc::new(move || {
            execution
                .request_settings()
                .map(|settings| settings.with_http_timeout(timeout_ms))
        })
    };
    let state = ApiState {
        admin,
        openai: OpenAiService::new(execution, lifecycle),
        health: HealthStatus::new(probes, worker_health),
    };
    let index = config.asset_directory.join("index.html");
    let mut router = Router::new()
        .route("/healthz", get(health::healthz))
        .merge(openai::router::router())
        .merge(provider::router())
        .merge(admin::router::<ApiState>())
        .merge(admin::model_router())
        .merge(auth::router::<ApiState>())
        .merge(key_usage::router::<ApiState>())
        .fallback_service(
            Router::new()
                .fallback_service(
                    ServeDir::new(config.asset_directory).fallback(ServeFile::new(index)),
                )
                .layer(axum::middleware::map_response(static_cache_control)),
        );
    if !config.cors_allowed_origins.is_empty() {
        let origins = config
            .cors_allowed_origins
            .iter()
            .map(|origin| {
                HeaderValue::from_str(origin)
                    .map_err(|_| ApiError::Config(ApiConfigError::InvalidCorsOrigin))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // credentials 模式禁止通配 allow_headers；显式列出鉴权与内容协商头
        router = router.layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::CONTENT_TYPE,
                    HeaderName::from_static("x-api-key"),
                    request_id_header.clone(),
                ])
                .allow_credentials(true),
        );
    }
    // Trace 必须在 SetRequestId 内侧，span 才能捕获本服务生成的 request_id；
    // 默认 DEBUG span 会被 info 日志过滤器丢弃，这里显式用 info_span
    let trace_layer = TraceLayer::new_for_http().make_span_with({
        let request_id_header = request_id_header.clone();
        move |request: &Request<_>| {
            let request_id = request
                .headers()
                .get(&request_id_header)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            tracing::info_span!(
                "request",
                request_id = %request_id,
                method = %request.method().as_str(),
                uri = %request.uri().path(),
            )
        }
    })
    .on_request(|_request: &Request<_>, _span: &tracing::Span| {
        tracing::info!(target: "request_trace", stage = "http.received", "HTTP request received");
    })
    .on_response(|response: &axum::http::Response<_>, latency: Duration, _span: &tracing::Span| {
        tracing::info!(target: "request_trace", stage = "http.response", status = response.status().as_u16(),
            headers_ms = latency.as_millis(), "HTTP response headers ready");
    });
    let router = router
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(trace_layer)
        .layer(SetRequestIdLayer::new(
            request_id_header.clone(),
            MakeRequestUuid,
        ))
        .with_state(state);
    Ok(ApiBundle {
        settings,
        router,
        middleware: None,
        timeout: config.request_timeout_seconds.map(Duration::from_secs),
        request_id_header,
    })
}

async fn static_cache_control(mut response: axum::response::Response) -> axum::response::Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    response
}

/// API 初始化失败的脱敏分类
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    Config(ApiConfigError),
}

#[derive(Clone)]
pub(crate) struct ApiState {
    admin: AdminServices,
    openai: OpenAiService,
    health: HealthStatus,
}

impl ApiState {
    #[must_use]
    pub(crate) const fn openai(&self) -> &OpenAiService {
        &self.openai
    }

    #[must_use]
    pub(crate) const fn health(&self) -> &HealthStatus {
        &self.health
    }
}

impl auth::SessionState for ApiState {
    fn admin_services(&self) -> &AdminServices {
        &self.admin
    }
}

fn validate_origin(raw: &str) -> Result<String, ApiConfigError> {
    let url = Url::parse(raw).map_err(|_| ApiConfigError::InvalidCorsOrigin)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiConfigError::InvalidCorsOrigin);
    }
    let origin = url.origin().ascii_serialization();
    HeaderValue::from_str(&origin).map_err(|_| ApiConfigError::InvalidCorsOrigin)?;
    Ok(origin)
}
