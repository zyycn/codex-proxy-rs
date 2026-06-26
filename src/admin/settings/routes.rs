//! 管理端设置处理器。

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    admin::auth::session::require_admin_session,
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    config::{
        settings::{AdminSettingsPatch, RuntimeSettingsError, SettingsServiceError},
        types::AppConfig,
    },
    http::middleware::request_id::RequestId,
    runtime::state::AppState,
};

const ALLOWED_SETTINGS_KEYS: [&str; 4] = [
    "defaultModel",
    "maxConcurrentPerAccount",
    "requestIntervalMs",
    "rotationStrategy",
];

/// 管理端设置响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSettingsData {
    pub default_model: String,
    pub max_concurrent_per_account: usize,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
}

impl AdminSettingsData {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            default_model: config.model.default_model.clone(),
            max_concurrent_per_account: config.auth.max_concurrent_per_account,
            request_interval_ms: config.auth.request_interval_ms,
            rotation_strategy: config.auth.rotation_strategy.clone(),
        }
    }
}

/// `GET /api/admin/settings`
pub async fn settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let config = state.services.settings.current();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config), request_id),
    ))
}

/// `POST /api/admin/settings`
pub async fn update_settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let patch = parse_settings_patch(&body, &request_id)?;
    let config = state
        .services
        .settings
        .update(patch)
        .await
        .map_err(|error| settings_service_error(error, &request_id))?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config), request_id),
    ))
}

fn parse_settings_patch(body: &[u8], request_id: &str) -> Result<AdminSettingsPatch, AdminError> {
    let value = serde_json::from_slice::<Value>(body).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40000,
            "Malformed JSON body",
            request_id,
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Settings payload must be an object",
            request_id,
        )
    })?;

    if let Some(key) = object
        .keys()
        .find(|key| !ALLOWED_SETTINGS_KEYS.contains(&key.as_str()))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("Unsupported settings field: {key}"),
            request_id,
        ));
    }

    serde_json::from_value(value).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid settings payload",
            request_id,
        )
    })
}

fn settings_service_error(error: RuntimeSettingsError, request_id: &str) -> AdminError {
    match error {
        RuntimeSettingsError::InvalidField(SettingsServiceError::InvalidField {
            field,
            message,
        }) => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("Invalid setting {field}: {message}"),
            request_id,
        ),
        RuntimeSettingsError::Persist(_) => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50000,
            "Failed to persist settings",
            request_id,
        ),
    }
}
