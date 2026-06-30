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
    admin::auth::session::require_admin_auth,
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
    require_admin_auth(&state, &headers).await?;
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
    require_admin_auth(&state, &headers).await?;

    let patch = parse_settings_patch(&body)?;
    let config = state
        .services
        .settings
        .update(patch)
        .await
        .map_err(settings_service_error)?;
    state
        .services
        .models
        .update_config(crate::upstream::models::ModelConfig {
            model_aliases: config.model_aliases.clone(),
        });

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminSettingsData::from_config(&config)),
    ))
}

/// `GET /api/admin/settings/admin-api-key`
pub(crate) async fn admin_api_key_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let status = state
        .services
        .settings
        .admin_api_key_status()
        .await
        .map_err(settings_service_error)?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(status),
    ))
}

/// `POST /api/admin/settings/admin-api-key/regenerate`
pub(crate) async fn regenerate_admin_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let key = state
        .services
        .settings
        .regenerate_admin_api_key()
        .await
        .map_err(settings_service_error)?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({ "key": key })),
    ))
}

/// `DELETE /api/admin/settings/admin-api-key`
pub(crate) async fn delete_admin_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    state
        .services
        .settings
        .delete_admin_api_key()
        .await
        .map_err(settings_service_error)?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({ "message": "Admin API key deleted" })),
    ))
}

fn parse_settings_patch(body: &[u8]) -> Result<AdminSettingsPatch, AdminError> {
    let value = serde_json::from_slice::<Value>(body)
        .map_err(|_| AdminError::malformed_json("Malformed JSON body"))?;
    let object = value
        .as_object()
        .ok_or_else(|| AdminError::bad_request("Settings payload must be an object"))?;

    if let Some(key) = object
        .keys()
        .find(|key| !ALLOWED_SETTINGS_KEYS.contains(&key.as_str()))
    {
        return Err(AdminError::bad_request(format!(
            "Unsupported settings field: {key}"
        )));
    }

    serde_json::from_value(value).map_err(|_| AdminError::bad_request("Invalid settings payload"))
}

fn settings_service_error(error: RuntimeSettingsError) -> AdminError {
    match error {
        RuntimeSettingsError::InvalidField(SettingsServiceError::InvalidField {
            field,
            message,
        }) => AdminError::bad_request(format!("Invalid setting {field}: {message}")),
        RuntimeSettingsError::Database(_)
        | RuntimeSettingsError::Json(_)
        | RuntimeSettingsError::StoredField { .. } => {
            AdminError::settings_persist_failed("Failed to persist settings")
        }
    }
}
