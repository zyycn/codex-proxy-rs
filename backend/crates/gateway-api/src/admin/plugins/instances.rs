use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use gateway_admin::model::plugins::instances::{
    ConfigurePluginInstance, PluginCapabilityBinding, PluginInstance, PluginInstanceRuntime,
    PluginInstanceRuntimeFailure, PluginInstanceRuntimeStatus, PluginInstanceView,
    RollbackPluginInstance,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{
        AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminResponse,
        wire::map_admin_service_error,
    },
    auth::SessionState,
};

pub(super) fn router<S: SessionState + Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/api/admin/plugins/instances",
            get(list::<S>).post(create::<S>),
        )
        .route("/api/admin/plugins/instances/update", post(configure::<S>))
        .route("/api/admin/plugins/instances/rollback", post(rollback::<S>))
        .route(
            "/api/admin/plugins/instances/switch-version",
            post(switch_version::<S>),
        )
        .route(
            "/api/admin/plugins/instances/version-plan",
            get(version_plan::<S>),
        )
        .route(
            "/api/admin/plugins/instances/rollback-plan",
            get(rollback_plan::<S>),
        )
        .route("/api/admin/plugins/instances/disable", post(disable::<S>))
        .route("/api/admin/plugins/instances/delete", post(remove::<S>))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceView {
    id: String,
    name: String,
    artifact_sha256: String,
    enabled: bool,
    configuration_required: bool,
    configuration: serde_json::Value,
    secret_fields: Vec<String>,
    bindings: Vec<PluginCapabilityBinding>,
    revision: u64,
    running: bool,
    published_revision: Option<u64>,
    runtime: InstanceRuntimeView,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum InstanceRuntimeStatus {
    Disabled,
    AwaitingPublication,
    Preparing,
    Running,
    Blocked,
    PreparationFailed,
    Faulted,
    Draining,
}

impl From<PluginInstanceRuntimeStatus> for InstanceRuntimeStatus {
    fn from(value: PluginInstanceRuntimeStatus) -> Self {
        match value {
            PluginInstanceRuntimeStatus::Disabled => Self::Disabled,
            PluginInstanceRuntimeStatus::AwaitingPublication => Self::AwaitingPublication,
            PluginInstanceRuntimeStatus::Preparing => Self::Preparing,
            PluginInstanceRuntimeStatus::Running => Self::Running,
            PluginInstanceRuntimeStatus::Blocked => Self::Blocked,
            PluginInstanceRuntimeStatus::PreparationFailed => Self::PreparationFailed,
            PluginInstanceRuntimeStatus::Faulted => Self::Faulted,
            PluginInstanceRuntimeStatus::Draining => Self::Draining,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceRuntimeFailure {
    code: String,
    message: String,
}

impl From<PluginInstanceRuntimeFailure> for InstanceRuntimeFailure {
    fn from(value: PluginInstanceRuntimeFailure) -> Self {
        Self {
            code: value.code,
            message: value.message,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceRuntimeView {
    status: InstanceRuntimeStatus,
    actual_revision: Option<u64>,
    actual_artifact_sha256: Option<String>,
    failure: Option<InstanceRuntimeFailure>,
    draining_revisions: Vec<u64>,
    retained_continuations: u32,
}

impl From<PluginInstanceRuntime> for InstanceRuntimeView {
    fn from(value: PluginInstanceRuntime) -> Self {
        Self {
            status: value.status.into(),
            actual_revision: value.actual_revision,
            actual_artifact_sha256: value.actual_artifact_sha256,
            failure: value.failure.map(Into::into),
            draining_revisions: value.draining_revisions,
            retained_continuations: value.retained_continuations,
        }
    }
}

impl From<PluginInstanceView> for InstanceView {
    fn from(value: PluginInstanceView) -> Self {
        let PluginInstance {
            id,
            name,
            artifact_sha256,
            enabled,
            trusted_process: _,
            configuration,
            secrets,
            grants: _,
            bindings,
            revision,
        } = value.instance;
        Self {
            id,
            name,
            artifact_sha256,
            enabled,
            configuration_required: value.configuration_required,
            configuration,
            secret_fields: secrets.into_keys().collect(),
            bindings,
            revision: revision.get(),
            running: value.running,
            published_revision: value.published_revision,
            runtime: value.runtime.into(),
        }
    }
}

async fn list<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError> {
    let instances: Vec<InstanceView> = state
        .admin_services()
        .plugins()
        .instances()
        .await
        .map_err(map_admin_service_error)?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(instances),
    ))
}

async fn create<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(input): AdminJson<ConfigurePluginInstance>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .configure_instance(None, input, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(
            serde_json::json!({"id":result.instance.id,"configRevision":result.config_revision.get()}),
        ),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureRequest {
    id: String,
    instance: ConfigurePluginInstance,
}

async fn configure<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ConfigureRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .configure_instance(
            Some(&request.id),
            request.instance,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            serde_json::json!({"id":result.instance.id,"configRevision":result.config_revision.get()}),
        ),
    ))
}

async fn disable<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<super::IdRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .disable_instance(&request.id, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision":result.config_revision.get()})),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackRequest {
    id: String,
    target: RollbackPluginInstance,
}

async fn rollback_plan<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    Query(request): Query<super::IdRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let plan = state
        .admin_services()
        .plugins()
        .rollback_plan(&request.id)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(plan)))
}

async fn rollback<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RollbackRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .rollback_instance(
            &request.id,
            request.target,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            serde_json::json!({"id":result.instance.id,"configRevision":result.config_revision.get()}),
        ),
    ))
}

async fn remove<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<super::IdRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let revision = state
        .admin_services()
        .plugins()
        .delete_instance(&request.id, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision":revision.get()})),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionPlanQuery {
    id: String,
    artifact_sha256: String,
}

async fn version_plan<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    Query(request): Query<VersionPlanQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let plan = state
        .admin_services()
        .plugins()
        .version_plan(&request.id, &request.artifact_sha256)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(plan)))
}

async fn switch_version<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RollbackRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .switch_instance_version(
            &request.id,
            request.target,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            serde_json::json!({"id":result.instance.id,"configRevision":result.config_revision.get()}),
        ),
    ))
}
