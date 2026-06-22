//! OpenAI 调试诊断工具。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};

use crate::{
    app::state::AppState,
    codex::transport::CodexRequestContext,
    http::middleware::request_id::RequestId,
    telemetry::diagnostics::{
        diagnostics_data, fingerprint_diagnostics, DiagnosticsInput, UpstreamProbeDiagnostics,
    },
};

fn forwarded_header_is_local(headers: &HeaderMap, name: &str) -> bool {
    let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    value.split(',').next().is_some_and(is_local_host)
}

fn is_local_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

pub fn is_local_debug_request(headers: &HeaderMap) -> bool {
    forwarded_header_is_local(headers, "x-forwarded-for")
        && forwarded_header_is_local(headers, "x-real-ip")
}

pub fn local_debug_forbidden_response() -> axum::response::Response {
    use serde_json::json;

    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "message": "debug endpoints are only available from localhost",
                "type": "forbidden"
            }
        })),
    )
        .into_response()
}

/// `GET /debug/diagnostics`
pub async fn diagnostics(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return local_debug_forbidden_response();
    }

    let config = state.services.settings.current();
    let accounts = state
        .services
        .accounts
        .list_pool_accounts()
        .await
        .unwrap_or_default();
    let capacity = state.services.account_pool.capacity_summary_now().await;
    (
        StatusCode::OK,
        Json(diagnostics_data(DiagnosticsInput {
            config: config.as_ref(),
            accounts: &accounts,
            capacity,
            fingerprint: &state.services.fingerprint,
        })),
    )
        .into_response()
}

/// `GET /debug/fingerprint`
pub async fn fingerprint(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return local_debug_forbidden_response();
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
        return local_debug_forbidden_response();
    }

    (
        StatusCode::OK,
        Json(probe_codex_models_endpoint(&state, request_id.as_str()).await),
    )
        .into_response()
}

async fn probe_codex_models_endpoint(
    state: &AppState,
    request_id: &str,
) -> UpstreamProbeDiagnostics {
    let config = state.services.settings.current();
    let context = CodexRequestContext {
        access_token: "",
        account_id: None,
        request_id,
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: None,
        installation_id: state.services.installation_id.as_deref(),
        session_id: None,
    };
    match state.services.codex.probe_connectivity(context).await {
        Ok(probe) => UpstreamProbeDiagnostics {
            target: "codexModels",
            backend_base_url: config.api.base_url.clone(),
            endpoint: probe.endpoint,
            reachable: !probe.status.is_server_error(),
            status_code: Some(probe.status.as_u16()),
            authorization: "unknown",
        },
        Err(_error) => UpstreamProbeDiagnostics {
            target: "codexModels",
            backend_base_url: config.api.base_url.clone(),
            endpoint: String::new(),
            reachable: false,
            status_code: None,
            authorization: "unknown",
        },
    }
}
