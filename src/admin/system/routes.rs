//! 管理端系统版本与在线更新路由。

use std::{env, time::Duration};

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    admin::auth::session::require_admin_auth,
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    runtime::state::AppState,
};

const DEFAULT_SERVICE_NAME: &str = "codex-proxy-rs";
const GITHUB_API_BASE: &str = "https://api.github.com/repos";

static SYSTEM_OPERATION_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckUpdateQuery {
    force: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest {
    target_version: Option<String>,
    confirm_backup: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionData {
    version: String,
    git_sha: String,
    build_time: String,
    deployment_mode: String,
    image: Option<String>,
    update_channel: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfoData {
    current_version: String,
    latest_version: String,
    has_update: bool,
    deployment_mode: String,
    release_url: Option<String>,
    notes: Option<String>,
    cached: bool,
    update_supported: bool,
    unsupported_reason: Option<String>,
    target_image: Option<String>,
    requires_backup: bool,
    warning: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStartedData {
    operation_id: String,
    deployment_mode: String,
    message: String,
    need_reconnect: bool,
    target_version: String,
    target_image: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
    prerelease: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdaterUpdateRequest {
    service: String,
    image: String,
    compose_project: Option<String>,
    target_version: String,
    confirm_backup: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdaterRollbackRequest {
    service: String,
    compose_project: Option<String>,
}

#[derive(Debug, Clone)]
struct SystemUpdateConfig {
    version: String,
    git_sha: String,
    build_time: String,
    deployment_mode: String,
    update_channel: String,
    update_repository: Option<String>,
    image_repository: Option<String>,
    image_tag: Option<String>,
    updater_url: Option<String>,
    updater_token: Option<String>,
    compose_project: Option<String>,
    compose_service: String,
}

/// `GET /api/admin/system/version`
pub(crate) async fn version(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let config = SystemUpdateConfig::from_env();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(config.version_data()),
    ))
}

/// `GET /api/admin/system/check-updates`
pub(crate) async fn check_updates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CheckUpdateQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let config = SystemUpdateConfig::from_env();
    let _force = query.force.unwrap_or(false);
    let info = check_latest_release(&config).await;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(info)))
}

/// `POST /api/admin/system/update`
pub(crate) async fn perform_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let _guard = SYSTEM_OPERATION_LOCK
        .try_lock()
        .map_err(|_| AdminError::conflict("System update already running"))?;
    let payload = parse_update_request(&body)?;
    let config = SystemUpdateConfig::from_env();
    let info = check_latest_release(&config).await;
    let reason = info.unsupported_reason.clone();
    if !info.update_supported {
        return Err(AdminError::conflict(reason.unwrap_or_else(|| {
            "System update is not supported in this deployment".into()
        })));
    }
    if !info.has_update && payload.target_version.is_none() {
        return Err(AdminError::conflict("Already up to date"));
    }
    if info.requires_backup && payload.confirm_backup != Some(true) {
        return Err(AdminError::conflict(
            "This update requires backup confirmation before continuing",
        ));
    }

    let target_version = payload
        .target_version
        .as_deref()
        .map(normalize_version_tag)
        .unwrap_or_else(|| normalize_version_tag(&info.latest_version));
    let target_image = config
        .target_image_for_version(&target_version)
        .ok_or_else(|| AdminError::conflict("Docker image repository is not configured"))?;

    call_updater_update(
        &config,
        UpdaterUpdateRequest {
            service: config.compose_service.clone(),
            image: target_image.clone(),
            compose_project: config.compose_project.clone(),
            target_version: target_version.clone(),
            confirm_backup: payload.confirm_backup.unwrap_or(false),
        },
    )
    .await?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdateStartedData {
            operation_id: format!("sysop-update-{}", chrono::Utc::now().timestamp_millis()),
            deployment_mode: config.deployment_mode,
            message: "Update started".to_string(),
            need_reconnect: true,
            target_version,
            target_image: Some(target_image),
        }),
    ))
}

/// `POST /api/admin/system/rollback`
pub(crate) async fn rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let _guard = SYSTEM_OPERATION_LOCK
        .try_lock()
        .map_err(|_| AdminError::conflict("System operation already running"))?;
    let config = SystemUpdateConfig::from_env();
    call_updater_rollback(
        &config,
        UpdaterRollbackRequest {
            service: config.compose_service.clone(),
            compose_project: config.compose_project.clone(),
        },
    )
    .await?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": "Rollback started",
            "needReconnect": true
        })),
    ))
}

/// `POST /api/admin/system/restart`
pub(crate) async fn restart(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    if env_string("CPR_ENABLE_SELF_RESTART").as_deref() != Some("true") {
        return Err(AdminError::conflict(
            "Self restart is disabled; set CPR_ENABLE_SELF_RESTART=true to enable it",
        ));
    }

    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::process::exit(0);
    });

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": "Restart scheduled",
            "needReconnect": true
        })),
    ))
}

impl SystemUpdateConfig {
    fn from_env() -> Self {
        let image_repository = env_string("CPR_IMAGE_REPOSITORY");
        let update_repository = env_string("CPR_UPDATE_REPOSITORY").or_else(|| {
            image_repository
                .as_deref()
                .and_then(github_repo_from_image)
                .map(str::to_string)
        });
        Self {
            version: env_string("CPR_VERSION").unwrap_or_else(|| {
                option_env!("CPR_VERSION")
                    .unwrap_or(env!("CARGO_PKG_VERSION"))
                    .to_string()
            }),
            git_sha: env_string("CPR_GIT_SHA")
                .unwrap_or_else(|| option_env!("CPR_GIT_SHA").unwrap_or("unknown").to_string()),
            build_time: env_string("CPR_BUILD_TIME").unwrap_or_else(|| {
                option_env!("CPR_BUILD_TIME")
                    .unwrap_or("unknown")
                    .to_string()
            }),
            deployment_mode: env_string("CPR_DEPLOYMENT_MODE")
                .unwrap_or_else(|| "source".to_string()),
            update_channel: env_string("CPR_UPDATE_CHANNEL")
                .unwrap_or_else(|| "stable".to_string()),
            update_repository,
            image_repository,
            image_tag: env_string("CPR_IMAGE_TAG"),
            updater_url: env_string("CPR_UPDATER_URL"),
            updater_token: env_string("CPR_UPDATER_TOKEN"),
            compose_project: env_string("CPR_COMPOSE_PROJECT"),
            compose_service: env_string("CPR_COMPOSE_SERVICE")
                .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string()),
        }
    }

    fn version_data(&self) -> VersionData {
        VersionData {
            version: self.version.clone(),
            git_sha: self.git_sha.clone(),
            build_time: self.build_time.clone(),
            deployment_mode: self.deployment_mode.clone(),
            image: self.current_image(),
            update_channel: self.update_channel.clone(),
        }
    }

    fn current_image(&self) -> Option<String> {
        let repository = self.image_repository.as_ref()?;
        let tag = self.image_tag.as_deref().unwrap_or(self.version.as_str());
        Some(format!("{repository}:{tag}"))
    }

    fn target_image_for_version(&self, version: &str) -> Option<String> {
        self.image_repository
            .as_ref()
            .map(|repository| format!("{repository}:{}", normalize_version_tag(version)))
    }

    fn update_support_error(&self) -> Option<String> {
        if self.deployment_mode != "docker" {
            return Some("One-click update requires CPR_DEPLOYMENT_MODE=docker".to_string());
        }
        if self.image_repository.is_none() {
            return Some("Docker one-click update requires CPR_IMAGE_REPOSITORY".to_string());
        }
        if self.updater_url.is_none() || self.updater_token.is_none() {
            return Some(
                "Docker one-click update requires CPR_UPDATER_URL and CPR_UPDATER_TOKEN"
                    .to_string(),
            );
        }
        None
    }
}

async fn check_latest_release(config: &SystemUpdateConfig) -> UpdateInfoData {
    let Some(repository) = config.update_repository.as_deref() else {
        return UpdateInfoData {
            current_version: config.version.clone(),
            latest_version: config.version.clone(),
            has_update: false,
            deployment_mode: config.deployment_mode.clone(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: false,
            unsupported_reason: Some(
                "Update checks require CPR_UPDATE_REPOSITORY or a GHCR image repository"
                    .to_string(),
            ),
            target_image: None,
            requires_backup: false,
            warning: None,
        };
    };

    match fetch_latest_release(repository).await {
        Ok(release) => update_info_from_release(config, release),
        Err(error) => UpdateInfoData {
            current_version: config.version.clone(),
            latest_version: config.version.clone(),
            has_update: false,
            deployment_mode: config.deployment_mode.clone(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: false,
            unsupported_reason: config.update_support_error(),
            target_image: None,
            requires_backup: false,
            warning: Some(error),
        },
    }
}

async fn fetch_latest_release(repository: &str) -> Result<GitHubRelease, String> {
    let url = format!("{GITHUB_API_BASE}/{repository}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "codex-proxy-rs-updater")
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub release check failed with {status}"));
    }
    response
        .json::<GitHubRelease>()
        .await
        .map_err(|error| error.to_string())
}

fn update_info_from_release(config: &SystemUpdateConfig, release: GitHubRelease) -> UpdateInfoData {
    let latest_version = normalize_version_tag(&release.tag_name);
    let has_update = release_allowed(config, &release)
        && version_is_newer(&config.version, &latest_version).unwrap_or(false);
    let unsupported_reason = config.update_support_error();
    let update_supported = unsupported_reason.is_none();
    let target_image = update_supported
        .then(|| config.target_image_for_version(&latest_version))
        .flatten();
    let notes = release.body.or(release.name);
    let requires_backup = notes
        .as_deref()
        .is_some_and(|notes| notes.to_ascii_lowercase().contains("backup required: yes"));

    UpdateInfoData {
        current_version: config.version.clone(),
        latest_version,
        has_update,
        deployment_mode: config.deployment_mode.clone(),
        release_url: release.html_url,
        notes,
        cached: false,
        update_supported,
        unsupported_reason,
        target_image,
        requires_backup,
        warning: None,
    }
}

fn release_allowed(config: &SystemUpdateConfig, release: &GitHubRelease) -> bool {
    config.update_channel != "stable" || !release.prerelease
}

fn version_is_newer(current: &str, latest: &str) -> Option<bool> {
    let current = Version::parse(&normalize_version_tag(current)).ok()?;
    let latest = Version::parse(&normalize_version_tag(latest)).ok()?;
    Some(latest > current)
}

async fn call_updater_update(
    config: &SystemUpdateConfig,
    payload: UpdaterUpdateRequest,
) -> Result<(), AdminError> {
    call_updater(config, "/update", &payload).await
}

async fn call_updater_rollback(
    config: &SystemUpdateConfig,
    payload: UpdaterRollbackRequest,
) -> Result<(), AdminError> {
    call_updater(config, "/rollback", &payload).await
}

async fn call_updater<T: Serialize>(
    config: &SystemUpdateConfig,
    path: &str,
    payload: &T,
) -> Result<(), AdminError> {
    let updater_url = config
        .updater_url
        .as_deref()
        .ok_or_else(|| AdminError::conflict("CPR_UPDATER_URL is not configured"))?;
    let token = config
        .updater_token
        .as_deref()
        .ok_or_else(|| AdminError::conflict("CPR_UPDATER_TOKEN is not configured"))?;
    let url = format!("{}{}", updater_url.trim_end_matches('/'), path);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| AdminError::internal("Failed to create updater client"))?;
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(payload)
        .send()
        .await
        .map_err(|error| AdminError::bad_gateway(format!("Updater request failed: {error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AdminError::bad_gateway(format!(
            "Updater returned {status}: {}",
            body.trim()
        )));
    }
    Ok(())
}

fn parse_update_request(body: &[u8]) -> Result<UpdateRequest, AdminError> {
    if body.trim_ascii().is_empty() {
        return Ok(UpdateRequest {
            target_version: None,
            confirm_backup: None,
        });
    }
    serde_json::from_slice(body).map_err(|_| AdminError::malformed_json("Malformed JSON body"))
}

fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_version_tag(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn github_repo_from_image(image_repository: &str) -> Option<&str> {
    image_repository.strip_prefix("ghcr.io/")
}
