use std::sync::Arc;

use axum::{
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;

use crate::{
    admin::auth::service::AdminAuthPoolSummary,
    codex::{accounts::pool::AccountCapacitySummary, gateway::fingerprint::model::Fingerprint},
    config::AppConfig,
    platform::http::middleware::RequestId,
    runtime::state::AppState,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticsData {
    status: &'static str,
    runtime: RuntimeDiagnostics,
    paths: PathDiagnostics,
    transport: TransportDiagnostics,
    accounts: AccountDiagnostics,
    settings: SettingsDiagnostics,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeDiagnostics {
    package_name: &'static str,
    package_version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PathDiagnostics {
    config: &'static str,
    local_config: String,
    database_url: String,
    logs_directory: String,
    master_key_file: String,
    api_key_pepper_file: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportDiagnostics {
    backend_base_url: String,
    tls: TlsDiagnostics,
    fingerprint: FingerprintDiagnostics,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TlsDiagnostics {
    force_http11: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintDiagnostics {
    source: &'static str,
    originator: String,
    app_version: String,
    build_number: String,
    platform: String,
    arch: String,
    chromium_version: String,
    user_agent: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountDiagnostics {
    repository_available: bool,
    authenticated_state: bool,
    pool: AccountPoolDiagnostics,
    capacity: CapacityDiagnostics,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountPoolDiagnostics {
    total: usize,
    active: usize,
    expired: usize,
    quota_exhausted: usize,
    refreshing: usize,
    disabled: usize,
    banned: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapacityDiagnostics {
    max_concurrent_per_account: usize,
    total_slots: usize,
    used_slots: usize,
    available_slots: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsDiagnostics {
    default_model: String,
    refresh_enabled: bool,
    rotation_strategy: String,
    quota_skip_exhausted: bool,
    logs_enabled: bool,
}

pub async fn diagnostics(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "debug endpoint is local-only" })),
        )
            .into_response();
    }

    (StatusCode::OK, Json(diagnostics_data(&state).await)).into_response()
}

pub(crate) async fn diagnostics_data(state: &AppState) -> DiagnosticsData {
    let config = state.services.settings.current().await;
    let auth_status = state.services.admin_auth.status().await.ok();
    let capacity = state.services.accounts.runtime_capacity_summary().await;
    let fingerprint = Fingerprint::default_codex_desktop();

    DiagnosticsData {
        status: "ok",
        runtime: RuntimeDiagnostics {
            package_name: env!("CARGO_PKG_NAME"),
            package_version: env!("CARGO_PKG_VERSION"),
        },
        paths: paths_diagnostics(state, &config),
        transport: transport_diagnostics(&config, fingerprint),
        accounts: AccountDiagnostics {
            repository_available: state.services.accounts.has_repository(),
            authenticated_state: auth_status
                .as_ref()
                .is_some_and(|status| status.authenticated),
            pool: auth_status
                .map(|status| AccountPoolDiagnostics::from(status.pool))
                .unwrap_or_default(),
            capacity: CapacityDiagnostics::from(capacity),
        },
        settings: SettingsDiagnostics::from(&config),
    }
}

pub async fn debug_fingerprint(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "debug endpoint is local-only" })),
        )
            .into_response();
    }

    // 从实际服务中获取指纹
    let fingerprint = state.services.responses.upstream_fingerprint();

    (
        StatusCode::OK,
        Json(fingerprint_diagnostics(fingerprint.clone())),
    )
        .into_response()
}

pub async fn debug_upstream(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "debug endpoint is local-only" })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(
            state
                .services
                .diagnostics
                .probe_upstream(request_id.as_str())
                .await,
        ),
    )
        .into_response()
}

fn paths_diagnostics(state: &AppState, config: &AppConfig) -> PathDiagnostics {
    PathDiagnostics {
        config: "config.yaml",
        local_config: state
            .services
            .settings
            .local_config_path()
            .display()
            .to_string(),
        database_url: config.database.url.clone(),
        logs_directory: config.logging.directory.clone(),
        master_key_file: config.security.master_key_file.clone(),
        api_key_pepper_file: config.security.api_key_pepper_file.clone(),
    }
}

fn transport_diagnostics(config: &AppConfig, fingerprint: Fingerprint) -> TransportDiagnostics {
    TransportDiagnostics {
        backend_base_url: config.api.base_url.clone(),
        tls: TlsDiagnostics {
            force_http11: config.tls.force_http11,
        },
        fingerprint: fingerprint_diagnostics(fingerprint),
    }
}

fn fingerprint_diagnostics(fingerprint: Fingerprint) -> FingerprintDiagnostics {
    let user_agent = fingerprint.user_agent();
    FingerprintDiagnostics {
        source: "staticDefault",
        originator: fingerprint.originator,
        app_version: fingerprint.app_version,
        build_number: fingerprint.build_number,
        platform: fingerprint.platform,
        arch: fingerprint.arch,
        chromium_version: fingerprint.chromium_version,
        user_agent,
    }
}

fn is_local_debug_request(headers: &HeaderMap) -> bool {
    forwarded_header_is_local(headers, "x-forwarded-for")
        && forwarded_header_is_local(headers, "x-real-ip")
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

impl From<AdminAuthPoolSummary> for AccountPoolDiagnostics {
    fn from(summary: AdminAuthPoolSummary) -> Self {
        Self {
            total: summary.total,
            active: summary.active,
            expired: summary.expired,
            quota_exhausted: summary.quota_exhausted,
            refreshing: summary.refreshing,
            disabled: summary.disabled,
            banned: summary.banned,
        }
    }
}

impl From<AccountCapacitySummary> for CapacityDiagnostics {
    fn from(summary: AccountCapacitySummary) -> Self {
        Self {
            max_concurrent_per_account: summary.max_concurrent_per_account,
            total_slots: summary.total_slots,
            used_slots: summary.used_slots,
            available_slots: summary.available_slots,
        }
    }
}

impl From<&Arc<AppConfig>> for SettingsDiagnostics {
    fn from(config: &Arc<AppConfig>) -> Self {
        Self {
            default_model: config.model.default_model.clone(),
            refresh_enabled: config.auth.refresh_enabled,
            rotation_strategy: config.auth.rotation_strategy.clone(),
            quota_skip_exhausted: config.quota.skip_exhausted,
            logs_enabled: config.logging.enabled,
        }
    }
}
