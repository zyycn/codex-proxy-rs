//! 顶层 HTTP 路由 —— 组合 OpenAI API、管理端 API 和静态资源服务。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use axum::{extract::State, http::StatusCode, middleware, routing::get, Router};

use crate::{
    accounts::{
        manage::AccountManageService, pool::RuntimeAccountPoolService,
        refresh::RuntimeTokenRefreshService, store::AccountStore,
    },
    api::{
        admin, assets, client,
        middleware::{request_id::attach_request_id, trace::http_trace_layer},
    },
    auth::service::SessionService,
    dispatch::{affinity::RuntimeSessionAffinityService, service::ResponseDispatchService},
    keys::{manage::KeyManageService, service::KeyVerifier},
    models::service::ModelService,
    settings::service::RuntimeSettingsService,
    telemetry::{
        account_usage::query::AccountUsageQueryService, ops::query::OpsQueryService,
        usage::query::UsageQueryService,
    },
    upstream::openai::{fingerprint::RuntimeFingerprint, token_client::OpenAiTokenClient},
};

/// HTTP API 所需的领域服务集合。
#[derive(Clone)]
pub struct ApiServices {
    pub health_probe: Arc<dyn HealthProbe>,
    pub models: Arc<ModelService>,
    pub accounts: Arc<dyn AccountStore>,
    pub client_keys: Arc<KeyVerifier>,
    pub admin_client_keys: Arc<KeyManageService>,
    pub admin_sessions: Arc<SessionService>,
    pub settings: Arc<RuntimeSettingsService>,
    pub admin_accounts: Arc<AccountManageService>,
    pub usage_records: Arc<UsageQueryService>,
    pub ops_errors: Arc<OpsQueryService>,
    pub usage: Arc<AccountUsageQueryService>,
    pub account_pool: Arc<RuntimeAccountPoolService>,
    pub token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    pub responses: Arc<ResponseDispatchService>,
    pub session_affinity: Arc<RuntimeSessionAffinityService>,
    pub fingerprint: RuntimeFingerprint,
    pub process_control: Arc<dyn ProcessControl>,
}

/// 由 bootstrap 实现的进程生命周期控制端口。
pub trait ProcessControl: Send + Sync + 'static {
    fn request_shutdown(&self);
    fn request_restart(&self, executable_path: PathBuf);
    fn subscribe_shutdown(&self) -> tokio::sync::broadcast::Receiver<()>;
}

/// 由 bootstrap 实现的基础设施健康检查端口。
#[async_trait]
pub trait HealthProbe: Send + Sync + 'static {
    async fn check(&self) -> Result<(), String>;
}

/// HTTP handler 共享状态。
#[derive(Clone)]
pub struct AppState {
    pub services: ApiServices,
}

/// 默认前端构建产物目录。
pub const DEFAULT_ASSET_DIST_DIR: &str = "web/dist";

/// 构造整个 HTTP 服务路由。
pub fn router() -> Router<AppState> {
    router_with_assets(DEFAULT_ASSET_DIST_DIR)
}

/// 使用指定前端构建产物目录构造整个 HTTP 服务路由。
pub fn router_with_assets(dist_dir: impl AsRef<Path>) -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(client::router::router())
        .merge(admin::router::router())
        .fallback_service(assets::spa_router(dist_dir))
        .layer(http_trace_layer())
        .layer(middleware::from_fn(attach_request_id))
}

async fn healthz(State(state): State<AppState>) -> StatusCode {
    if let Err(error) = state.services.health_probe.check().await {
        tracing::warn!(error, "health check failed");
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::NO_CONTENT
}
