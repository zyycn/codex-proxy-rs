use std::{
    collections::BTreeMap,
    env, fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = UpdaterConfig::from_env()?;
    let bind = config.bind;
    let app = Router::new()
        .route("/update", post(update))
        .route("/rollback", post(rollback))
        .with_state(config);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct UpdaterConfig {
    bind: SocketAddr,
    token: String,
    allowed_image_repository: String,
    docker_bin: String,
    docker_compose_bin: Option<String>,
    compose_file: PathBuf,
    compose_project: Option<String>,
    compose_env_file: Option<PathBuf>,
    compose_image_env: String,
    state_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRequest {
    service: String,
    image: String,
    compose_project: Option<String>,
    target_version: Option<String>,
    confirm_backup: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollbackRequest {
    service: String,
    compose_project: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UpdaterState {
    previous_image: Option<String>,
    current_image: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdaterResponse {
    message: String,
    service: String,
    image: Option<String>,
}

#[derive(Debug)]
struct UpdaterError {
    status: StatusCode,
    message: String,
}

impl UpdaterConfig {
    fn from_env() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bind = env_string("CPR_UPDATER_BIND")
            .unwrap_or_else(|| "0.0.0.0:8090".to_string())
            .parse::<SocketAddr>()?;
        let token = required_env("CPR_UPDATER_TOKEN")?;
        let allowed_image_repository = required_env("CPR_ALLOWED_IMAGE_REPOSITORY")?;
        let docker_bin = env_string("CPR_DOCKER_BIN").unwrap_or_else(|| "docker".to_string());
        let docker_compose_bin = env_string("CPR_DOCKER_COMPOSE_BIN");
        let compose_file = PathBuf::from(
            env_string("CPR_COMPOSE_FILE")
                .unwrap_or_else(|| "/workspace/docker-compose.yml".into()),
        );
        let compose_project = env_string("CPR_COMPOSE_PROJECT");
        let compose_env_file = env_string("CPR_COMPOSE_ENV_FILE").map(PathBuf::from);
        let compose_image_env =
            env_string("CPR_COMPOSE_IMAGE_ENV").unwrap_or_else(|| "CPR_IMAGE".to_string());
        let state_file = PathBuf::from(
            env_string("CPR_UPDATER_STATE_FILE")
                .unwrap_or_else(|| "/tmp/codex-proxy-rs-updater-state.json".to_string()),
        );
        Ok(Self {
            bind,
            token,
            allowed_image_repository,
            docker_bin,
            docker_compose_bin,
            compose_file,
            compose_project,
            compose_env_file,
            compose_image_env,
            state_file,
        })
    }

    fn project_for(&self, requested: Option<&str>) -> Option<String> {
        requested
            .and_then(non_empty)
            .map(ToString::to_string)
            .or_else(|| self.compose_project.clone())
    }
}

async fn update(
    State(config): State<UpdaterConfig>,
    headers: HeaderMap,
    Json(payload): Json<UpdateRequest>,
) -> Result<impl IntoResponse, UpdaterError> {
    require_token(&config, &headers)?;
    let service = validate_name("service", &payload.service)?;
    let image = validate_image(&config, &payload.image)?;
    let _target_version = payload.target_version.as_deref().and_then(non_empty);
    let _confirm_backup = payload.confirm_backup.unwrap_or(false);

    let compose_project = payload.compose_project;
    let task_config = config;
    let task_service = service.clone();
    let task_image = image.clone();
    tokio::spawn(async move {
        if let Err(error) = run_update(
            &task_config,
            &task_service,
            &task_image,
            compose_project.as_deref(),
        )
        .await
        {
            eprintln!("updater update failed: {}", error.message);
        }
    });

    Ok(Json(UpdaterResponse {
        message: "update started".to_string(),
        service,
        image: Some(image),
    }))
}

async fn rollback(
    State(config): State<UpdaterConfig>,
    headers: HeaderMap,
    Json(payload): Json<RollbackRequest>,
) -> Result<impl IntoResponse, UpdaterError> {
    require_token(&config, &headers)?;
    let service = validate_name("service", &payload.service)?;
    let state = read_state(&config.state_file)?;
    let image = state
        .previous_image
        .ok_or_else(|| UpdaterError::conflict("No previous image recorded for rollback"))?;
    let image = validate_image(&config, &image)?;

    let compose_project = payload.compose_project;
    let task_config = config;
    let task_service = service.clone();
    let task_image = image.clone();
    tokio::spawn(async move {
        if let Err(error) = run_update(
            &task_config,
            &task_service,
            &task_image,
            compose_project.as_deref(),
        )
        .await
        {
            eprintln!("updater rollback failed: {}", error.message);
        }
    });

    Ok(Json(UpdaterResponse {
        message: "rollback started".to_string(),
        service,
        image: Some(image),
    }))
}

async fn run_update(
    config: &UpdaterConfig,
    service: &str,
    image: &str,
    compose_project: Option<&str>,
) -> Result<(), UpdaterError> {
    let previous_image = current_image(config)?;
    run_command(config.docker_command(["pull", image]), "docker pull").await?;
    if let Some(env_file) = &config.compose_env_file {
        upsert_env_file(env_file, &config.compose_image_env, image)?;
    }
    let state = UpdaterState {
        previous_image,
        current_image: Some(image.to_string()),
    };
    write_state(&config.state_file, &state)?;

    let mut args = vec![
        "-f".to_string(),
        config.compose_file.to_string_lossy().to_string(),
    ];
    if let Some(project) = config.project_for(compose_project) {
        args.push("-p".to_string());
        args.push(project);
    }
    args.extend(["up".to_string(), "-d".to_string(), service.to_string()]);
    run_command(config.compose_command(args), "docker compose up").await
}

impl UpdaterConfig {
    fn docker_command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut command = Command::new(&self.docker_bin);
        for arg in args {
            command.arg(arg.as_ref());
        }
        command
    }

    fn compose_command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if let Some(compose_bin) = &self.docker_compose_bin {
            let mut command = Command::new(compose_bin);
            for arg in args {
                command.arg(arg.as_ref());
            }
            return command;
        }

        let mut command = Command::new(&self.docker_bin);
        command.arg("compose");
        for arg in args {
            command.arg(arg.as_ref());
        }
        command
    }
}

async fn run_command(mut command: Command, label: &'static str) -> Result<(), UpdaterError> {
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .map_err(|error| UpdaterError::internal(format!("{label} join error: {error}")))?
        .map_err(|error| UpdaterError::internal(format!("{label} failed to start: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(UpdaterError::internal(format!(
        "{label} failed: {}{}",
        stderr.trim(),
        stdout.trim()
    )))
}

fn current_image(config: &UpdaterConfig) -> Result<Option<String>, UpdaterError> {
    let Some(env_file) = &config.compose_env_file else {
        return Ok(read_state(&config.state_file)?.current_image);
    };
    if !env_file.exists() {
        return Ok(read_state(&config.state_file)?.current_image);
    }
    let values = read_env_file(env_file)?;
    Ok(values.get(&config.compose_image_env).cloned().or_else(|| {
        read_state(&config.state_file)
            .ok()
            .and_then(|state| state.current_image)
    }))
}

fn read_state(path: &Path) -> Result<UpdaterState, UpdaterError> {
    if !path.exists() {
        return Ok(UpdaterState::default());
    }
    let data = fs::read_to_string(path).map_err(|error| {
        UpdaterError::internal(format!("Failed to read updater state: {error}"))
    })?;
    serde_json::from_str(&data)
        .map_err(|error| UpdaterError::internal(format!("Invalid updater state: {error}")))
}

fn write_state(path: &Path, state: &UpdaterState) -> Result<(), UpdaterError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            UpdaterError::internal(format!("Failed to create updater state directory: {error}"))
        })?;
    }
    let data = serde_json::to_string_pretty(state).map_err(|error| {
        UpdaterError::internal(format!("Failed to encode updater state: {error}"))
    })?;
    fs::write(path, data)
        .map_err(|error| UpdaterError::internal(format!("Failed to write updater state: {error}")))
}

fn upsert_env_file(path: &Path, key: &str, value: &str) -> Result<(), UpdaterError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            UpdaterError::internal(format!("Failed to create env dir: {error}"))
        })?;
    }
    let mut values = if path.exists() {
        read_env_file(path)?
    } else {
        BTreeMap::new()
    };
    values.insert(key.to_string(), value.to_string());
    let data = values
        .into_iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect::<String>();
    fs::write(path, data)
        .map_err(|error| UpdaterError::internal(format!("Failed to write env file: {error}")))
}

fn read_env_file(path: &Path) -> Result<BTreeMap<String, String>, UpdaterError> {
    let data = fs::read_to_string(path)
        .map_err(|error| UpdaterError::internal(format!("Failed to read env file: {error}")))?;
    let mut values = BTreeMap::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(values)
}

fn require_token(config: &UpdaterConfig, headers: &HeaderMap) -> Result<(), UpdaterError> {
    let Some(header) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(UpdaterError::unauthorized("Missing authorization header"));
    };
    let Ok(value) = header.to_str() else {
        return Err(UpdaterError::unauthorized("Invalid authorization header"));
    };
    let token = value.strip_prefix("Bearer ").unwrap_or_default().trim();
    if token == config.token {
        return Ok(());
    }
    Err(UpdaterError::unauthorized("Invalid updater token"))
}

fn validate_image(config: &UpdaterConfig, image: &str) -> Result<String, UpdaterError> {
    let image = validate_name("image", image)?;
    let allowed_prefix = format!("{}:", config.allowed_image_repository);
    let allowed_digest_prefix = format!("{}@", config.allowed_image_repository);
    if image.starts_with(&allowed_prefix) || image.starts_with(&allowed_digest_prefix) {
        return Ok(image);
    }
    Err(UpdaterError::bad_request(
        "Image repository is not allowed for update",
    ))
}

fn validate_name(field: &str, value: &str) -> Result<String, UpdaterError> {
    let Some(value) = non_empty(value) else {
        return Err(UpdaterError::bad_request(format!(
            "{field} must not be empty"
        )));
    };
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(UpdaterError::bad_request(format!(
            "{field} must not contain whitespace or control characters"
        )));
    }
    Ok(value.to_string())
}

fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_env(key: &str) -> Result<String, io::Error> {
    env_string(key)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{key} is required")))
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

impl UpdaterError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for UpdaterError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({
                "message": self.message
            })),
        )
            .into_response()
    }
}
