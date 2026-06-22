//! OpenAI API 路由与诊断处理器。

pub mod chat;
pub mod diagnostics;
pub mod errors;
pub mod models;
pub mod responses;
pub mod sse;

use axum::{
    routing::{get, post},
    Router,
};

use crate::app::state::AppState;

use self::{
    chat::chat_completions,
    models::{debug_models, model_catalog, model_detail, model_info, models},
    responses::{compact_responses, responses, review_responses},
};

/// 构造 OpenAI 兼容 API 路由。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/responses", post(responses))
        .route("/v1/responses/review", post(review_responses))
        .route("/v1/responses/compact", post(compact_responses))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/v1/models/catalog", get(model_catalog))
        .route("/v1/models/{model_id}", get(model_detail))
        .route("/v1/models/{model_id}/info", get(model_info))
        .route("/debug/models", get(debug_models))
        .route("/debug/diagnostics", get(diagnostics))
        .route("/debug/fingerprint", get(fingerprint))
        .route("/debug/upstream", get(upstream))
}

// ---------------------------------------------------------------------------
// 诊断处理器
// ---------------------------------------------------------------------------

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    accounts::{
        model::{Account, AccountStatus},
        pool::AccountCapacitySummary,
    },
    codex::fingerprint::Fingerprint,
    config::types::AppConfig,
    http::middleware::request_id::RequestId,
};

const LOCAL_DEBUG_ONLY_ERROR: &str = "debug endpoint is local-only";

/// 诊断数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsData {
    pub status: &'static str,
    pub runtime: RuntimeDiagnostics,
    pub paths: PathDiagnostics,
    pub transport: TransportDiagnostics,
    pub accounts: AccountDiagnostics,
    pub settings: SettingsDiagnostics,
}

/// Runtime path diagnostics.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDiagnostics {
    pub config: &'static str,
    pub database_url: String,
}

/// 运行时包信息。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDiagnostics {
    pub package_name: &'static str,
    pub package_version: &'static str,
}

/// 上游传输配置。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportDiagnostics {
    pub backend_base_url: String,
    pub tls: TlsDiagnostics,
    pub fingerprint: FingerprintDiagnostics,
}

/// TLS 配置。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsDiagnostics {
    pub force_http11: bool,
}

/// 账号诊断数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDiagnostics {
    pub repository_available: bool,
    pub pool: AccountPoolDiagnostics,
    pub capacity: AccountCapacityDiagnostics,
}

/// 账号池摘要。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPoolDiagnostics {
    pub total: usize,
    pub active: usize,
    pub expired: usize,
    pub quota_exhausted: usize,
    pub refreshing: usize,
    pub disabled: usize,
    pub banned: usize,
}

/// Account-pool capacity diagnostics.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCapacityDiagnostics {
    pub max_concurrent_per_account: usize,
    pub total_slots: usize,
    pub used_slots: usize,
    pub available_slots: usize,
}

/// Runtime fingerprint summary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintDiagnostics {
    pub source: &'static str,
    pub originator: String,
    pub app_version: String,
    pub build_number: String,
    pub platform: String,
    pub arch: String,
    pub chromium_version: String,
    pub user_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Upstream probe diagnostics.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProbeDiagnostics {
    pub target: &'static str,
    pub backend_base_url: String,
    pub endpoint: String,
    pub reachable: bool,
    pub status_code: Option<u16>,
    pub authorization: &'static str,
}

/// 主要运行设置。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDiagnostics {
    pub default_model: String,
    pub refresh_enabled: bool,
    pub rotation_strategy: String,
    pub quota_skip_exhausted: bool,
    pub logs_enabled: bool,
}

/// `GET /debug/diagnostics`
pub async fn diagnostics(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return local_debug_forbidden_response().into_response();
    }

    (StatusCode::OK, Json(diagnostics_data(&state).await)).into_response()
}

/// `GET /debug/fingerprint`
pub async fn fingerprint(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return local_debug_forbidden_response().into_response();
    }

    (
        StatusCode::OK,
        Json(fingerprint_diagnostics(&state.services.fingerprint)),
    )
        .into_response()
}

/// `GET /debug/upstream`
pub async fn upstream(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return local_debug_forbidden_response().into_response();
    }

    let probe = state
        .services
        .probe_codex_models_endpoint(request_id.as_str())
        .await;

    (
        StatusCode::OK,
        Json(UpstreamProbeDiagnostics {
            target: probe.target,
            backend_base_url: probe.backend_base_url,
            endpoint: probe.endpoint,
            reachable: probe.reachable,
            status_code: probe.status_code,
            authorization: probe.authorization,
        }),
    )
        .into_response()
}

/// 构造诊断数据。
pub async fn diagnostics_data(state: &AppState) -> DiagnosticsData {
    let config = state.services.settings.current();
    let accounts = state
        .services
        .accounts
        .list_pool_accounts()
        .await
        .unwrap_or_default();
    let capacity = state.services.account_pool.capacity_summary_now().await;
    DiagnosticsData {
        status: "ok",
        runtime: RuntimeDiagnostics {
            package_name: env!("CARGO_PKG_NAME"),
            package_version: env!("CARGO_PKG_VERSION"),
        },
        paths: PathDiagnostics {
            config: "config.yaml",
            database_url: config.database.url.clone(),
        },
        transport: transport_diagnostics(&config, &state.services.fingerprint),
        accounts: AccountDiagnostics {
            repository_available: true,
            pool: account_pool_diagnostics(&accounts),
            capacity: AccountCapacityDiagnostics::from(capacity),
        },
        settings: SettingsDiagnostics::from(config.as_ref()),
    }
}

fn transport_diagnostics(config: &AppConfig, fingerprint: &Fingerprint) -> TransportDiagnostics {
    TransportDiagnostics {
        backend_base_url: config.api.base_url.clone(),
        tls: TlsDiagnostics {
            force_http11: config.tls.force_http11,
        },
        fingerprint: fingerprint_diagnostics(fingerprint),
    }
}

fn account_pool_diagnostics(accounts: &[Account]) -> AccountPoolDiagnostics {
    let mut summary = AccountPoolDiagnostics {
        total: accounts.len(),
        ..AccountPoolDiagnostics::default()
    };
    for account in accounts {
        match account.status {
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Refreshing => summary.refreshing += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
}

fn fingerprint_diagnostics(fingerprint: &Fingerprint) -> FingerprintDiagnostics {
    FingerprintDiagnostics {
        source: "runtime",
        originator: fingerprint.originator.clone(),
        app_version: fingerprint.app_version.clone(),
        build_number: fingerprint.build_number.clone(),
        platform: fingerprint.platform.clone(),
        arch: fingerprint.arch.clone(),
        chromium_version: fingerprint.chromium_version.clone(),
        user_agent: fingerprint.user_agent(),
        updated_at: fingerprint.updated_at.clone(),
    }
}

pub(super) fn is_local_debug_request(headers: &HeaderMap) -> bool {
    forwarded_header_is_local(headers, "x-forwarded-for")
        && forwarded_header_is_local(headers, "x-real-ip")
}

pub(super) fn local_debug_forbidden_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": LOCAL_DEBUG_ONLY_ERROR })),
    )
}

impl From<AccountCapacitySummary> for AccountCapacityDiagnostics {
    fn from(summary: AccountCapacitySummary) -> Self {
        Self {
            max_concurrent_per_account: summary.max_concurrent_per_account,
            total_slots: summary.total_slots,
            used_slots: summary.used_slots,
            available_slots: summary.available_slots,
        }
    }
}

fn forwarded_header_is_local(headers: &HeaderMap, name: &str) -> bool {
    let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    value.split(',').next().is_some_and(is_local_host)
}

fn is_local_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

impl From<&AppConfig> for SettingsDiagnostics {
    fn from(config: &AppConfig) -> Self {
        Self {
            default_model: config.model.default_model.clone(),
            refresh_enabled: config.auth.refresh_enabled,
            rotation_strategy: config.auth.rotation_strategy.clone(),
            quota_skip_exhausted: config.quota.skip_exhausted,
            logs_enabled: config.logging.enabled,
        }
    }
}
