//! 管理端设置处理器。

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{
    admin::auth::session::require_admin_session,
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    config::{
        settings::{AdminSettingsPatch, RuntimeSettingsError, SettingsServiceError},
        types::AppConfig,
    },
    runtime::state::AppState,
};

const ALLOWED_SETTINGS_KEYS: [&str; 7] = [
    "modelAliases",
    "modelAccountRoutes",
    "refreshMarginSeconds",
    "refreshConcurrency",
    "maxConcurrentPerAccount",
    "requestIntervalMs",
    "rotationStrategy",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminSettingsData {
    model_aliases: BTreeMap<String, String>,
    model_account_routes: BTreeMap<String, Vec<String>>,
    refresh_margin_seconds: u64,
    refresh_concurrency: u32,
    max_concurrent_per_account: usize,
    request_interval_ms: u64,
    rotation_strategy: String,
}

impl AdminSettingsData {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            model_aliases: config.model_aliases.clone(),
            model_account_routes: config.model_account_routes.clone(),
            refresh_margin_seconds: config.auth.refresh_margin_seconds,
            refresh_concurrency: config.auth.refresh_concurrency,
            max_concurrent_per_account: config.auth.max_concurrent_per_account,
            request_interval_ms: config.auth.request_interval_ms,
            rotation_strategy: config.auth.rotation_strategy.clone(),
        }
    }
}

/// `GET /api/admin/settings`
pub(crate) async fn settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_session(&state, &headers).await?;
    let config = state.services.settings.current();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config)),
    ))
}

/// `POST /api/admin/settings`
pub(crate) async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_session(&state, &headers).await?;

    let patch = parse_settings_patch(&body)?;
    let config = state
        .services
        .settings
        .update(patch)
        .await
        .map_err(settings_service_error)?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config)),
    ))
}

fn parse_settings_patch(body: &[u8]) -> Result<AdminSettingsPatch, AdminError> {
    let value = serde_json::from_slice::<Value>(body)
        .map_err(|_| AdminError::new(StatusCode::BAD_REQUEST, 40000, "Malformed JSON body"))?;
    let object = value.as_object().ok_or_else(|| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Settings payload must be an object",
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
        ));
    }

    serde_json::from_value(value)
        .map_err(|_| AdminError::new(StatusCode::BAD_REQUEST, 40001, "Invalid settings payload"))
}

fn settings_service_error(error: RuntimeSettingsError) -> AdminError {
    match error {
        RuntimeSettingsError::InvalidField(SettingsServiceError::InvalidField {
            field,
            message,
        }) => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            format!("Invalid setting {field}: {message}"),
        ),
        RuntimeSettingsError::Database(_)
        | RuntimeSettingsError::Json(_)
        | RuntimeSettingsError::StoredField { .. } => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50000,
            "Failed to persist settings",
        ),
    }
}
