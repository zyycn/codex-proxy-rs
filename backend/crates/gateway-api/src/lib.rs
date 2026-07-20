//! 客户端与管理端 HTTP 协议 adapter。
//!
//! 本 crate 只负责请求解码、Core/Admin 调用和 HTTP/WS/SSE delivery。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::routing::get;
use gateway_admin::AdminServices;
use gateway_core::engine::execution::ExecutionService;
use gateway_core::health::{HealthProbe, WorkerHealthSource};
use gateway_core::lifecycle::ConnectionLifecycle;
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use url::Url;

const WEB_DIST_ENV: &str = "CPR_WEB_DIST_DIR";

use crate::health::HealthStatus;
use crate::openai::service::OpenAiService;

pub mod admin;
mod health;
pub mod openai;

/// API-owned HTTP 与静态资源配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    pub asset_directory: PathBuf,
    pub cors_allowed_origins: Vec<String>,
    pub request_timeout_seconds: Option<u64>,
    pub request_id_header: String,
}

impl ApiConfig {
    /// 解析静态资源相对路径并校验全部 HTTP 配置。
    ///
    /// # Errors
    ///
    /// 路径为空、origin/header 非法或 timeout 为零时返回脱敏错误。
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
        if self.asset_directory.as_os_str().is_empty() {
            return Err(ApiConfigError::InvalidAssetDirectory);
        }
        if self.asset_directory.is_relative() {
            self.asset_directory = source_dir.join(&self.asset_directory);
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

/// API 配置非法的稳定分类。
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

/// 完成组装的唯一 API router。
pub struct ApiBundle {
    router: Router,
}

impl ApiBundle {
    pub fn router(self) -> Router {
        self.router
    }
}

/// 组装客户端、管理端、健康检查和静态资源路由。
pub fn initialize(
    mut config: ApiConfig,
    execution: Arc<dyn ExecutionService>,
    admin: AdminServices,
    probes: Vec<Arc<dyn HealthProbe>>,
    worker_health: Arc<dyn WorkerHealthSource>,
    lifecycle: Arc<dyn ConnectionLifecycle>,
) -> Result<ApiBundle, ApiError> {
    config
        .resolve_and_validate(Path::new("."))
        .map_err(ApiError::Config)?;
    let request_id_header = HeaderName::from_str(&config.request_id_header)
        .map_err(|_| ApiError::Config(ApiConfigError::InvalidRequestIdHeader))?;
    let state = ApiState {
        admin,
        openai: OpenAiService::new(execution, lifecycle),
        health: HealthStatus::new(probes, worker_health),
    };
    let index = config.asset_directory.join("index.html");
    let mut router = Router::new()
        .route("/healthz", get(health::healthz))
        .merge(openai::router::router())
        .merge(admin::router::<ApiState>())
        .fallback_service(ServeDir::new(config.asset_directory).fallback(ServeFile::new(index)));
    if !config.cors_allowed_origins.is_empty() {
        let origins = config
            .cors_allowed_origins
            .iter()
            .map(|origin| {
                HeaderValue::from_str(origin)
                    .map_err(|_| ApiError::Config(ApiConfigError::InvalidCorsOrigin))
            })
            .collect::<Result<Vec<_>, _>>()?;
        router = router.layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(Any)
                .allow_credentials(true),
        );
    }
    if let Some(seconds) = config.request_timeout_seconds {
        router = router.layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(seconds),
        ));
    }
    let router = router
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    Ok(ApiBundle { router })
}

/// API 初始化失败的脱敏分类。
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

impl admin::AdminSessionState for ApiState {
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
