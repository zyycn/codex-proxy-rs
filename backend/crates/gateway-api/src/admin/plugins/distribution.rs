//! 插件更新来源、下载凭据与远程发行安装的 HTTP 接口

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use gateway_admin::model::plugins::distribution::{
    DownloadPurpose, GithubReleaseQuery, RemotePluginInstall, RemotePluginVerify,
    SourceAuthentication, SourceCredentialInfo,
};
use serde::Deserialize;

use super::artifacts::{InstallResultView, VerifiedArtifactView};
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
            "/api/admin/plugins/update-sources",
            get(update_sources::<S>).post(change_update_source::<S>),
        )
        .route("/api/admin/plugins/releases/query", post(query::<S>))
        .route("/api/admin/plugins/updates/check", post(check_update::<S>))
        .route("/api/admin/plugins/artifacts/install", post(install::<S>))
        .route("/api/admin/plugins/artifacts/verify", post(verify::<S>))
        .route(
            "/api/admin/plugins/source-credentials",
            get(credentials::<S>).post(create_credential::<S>),
        )
        .route(
            "/api/admin/plugins/source-credentials/delete",
            post(remove_credential::<S>),
        )
}

async fn update_sources<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError> {
    let sources = state
        .admin_services()
        .plugins()
        .update_sources()
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(sources),
    ))
}

async fn change_update_source<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(binding): AdminJson<gateway_admin::model::plugins::distribution::PluginSourceBinding>,
) -> Result<impl IntoResponse, AdminError> {
    let revision = state
        .admin_services()
        .plugins()
        .change_update_source(binding, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision":revision.get()})),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseQueryRequest {
    query: GithubReleaseQuery,
    #[serde(default)]
    credential_ids: Vec<String>,
    #[serde(default)]
    outbound_proxy_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateCheckRequest {
    plugin_id: String,
    #[serde(default)]
    credential_ids: Vec<String>,
}

async fn check_update<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateCheckRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let check = state
        .admin_services()
        .plugins()
        .check_update(&request.plugin_id, &request.credential_ids)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(check)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialRequest {
    name: String,
    origin: String,
    path_prefix: String,
    purposes: Vec<DownloadPurpose>,
    authentication: SourceAuthentication,
}

async fn query<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ReleaseQueryRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let release = state
        .admin_services()
        .plugins()
        .query_release(
            request.query,
            &request.credential_ids,
            request.outbound_proxy_id.as_deref(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(release),
    ))
}

async fn verify<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RemotePluginVerify>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .verify_remote(request)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(VerifiedArtifactView::new(result)),
    ))
}

async fn install<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RemotePluginInstall>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .install_remote(request, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(InstallResultView::new(result)),
    ))
}

async fn credentials<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError> {
    let credentials = state
        .admin_services()
        .plugins()
        .source_credentials()
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(credentials),
    ))
}

async fn create_credential<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<CredentialRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let info = SourceCredentialInfo {
        id: String::new(),
        name: request.name,
        origin: request.origin,
        path_prefix: request.path_prefix,
        purposes: request.purposes,
    };
    let info = state
        .admin_services()
        .plugins()
        .create_source_credential(
            info,
            request.authentication,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(info),
    ))
}

async fn remove_credential<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<super::IdRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let revision = state
        .admin_services()
        .plugins()
        .delete_source_credential(&request.id, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision":revision.get()})),
    ))
}
