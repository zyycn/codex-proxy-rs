//! 插件制品列表、图标、上传验证与安装管理的 HTTP 接口

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State, rejection::BytesRejection},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use gateway_admin::model::plugins::{
    InstalledPluginArtifact, PluginArtifactMetadata, PluginIconTheme, PluginInstallResult,
    PluginSource, distribution::VerifiedPluginArtifact,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::{
        AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse,
        wire::map_admin_service_error,
    },
    auth::SessionState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ArtifactView {
    metadata: PluginArtifactMetadata,
    source: PluginSource,
    installed_at: String,
    accepted_at: Option<String>,
}

impl ArtifactView {
    fn new(value: InstalledPluginArtifact) -> Self {
        Self {
            metadata: value.metadata,
            source: value.source,
            installed_at: value.installed_at.to_rfc3339(),
            accepted_at: value.accepted_at.map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Serialize)]
pub(super) struct VerifiedArtifactView {
    metadata: PluginArtifactMetadata,
    source: PluginSource,
}

impl VerifiedArtifactView {
    pub(super) fn new(value: VerifiedPluginArtifact) -> Self {
        Self {
            metadata: value.metadata,
            source: value.source,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InstallResultView {
    artifact: ArtifactView,
    config_revision: u64,
    default_instance_id: Option<String>,
    configuration_required: bool,
}

impl InstallResultView {
    pub(super) fn new(value: PluginInstallResult) -> Self {
        Self {
            artifact: ArtifactView::new(value.mutation.artifact),
            config_revision: value.mutation.config_revision.get(),
            default_instance_id: value.default_instance_id,
            configuration_required: value.configuration_required,
        }
    }
}

pub(in crate::admin) fn router<S: SessionState + Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/api/admin/plugins/artifacts", get(list::<S>))
        .route("/api/admin/plugins/artifacts/{sha256}/icon", get(icon::<S>))
        .route(
            "/api/admin/plugins/artifacts/upload",
            post(upload::<S>).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/api/admin/plugins/artifacts/upload/verify",
            post(verify_upload::<S>).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/api/admin/plugins/artifacts/accept", post(accept::<S>))
        .route("/api/admin/plugins/artifacts/delete", post(remove::<S>))
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
enum IconTheme {
    Light,
    Dark,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IconQuery {
    theme: IconTheme,
}

async fn icon<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    Path(sha256): Path<String>,
    AdminQuery(query): AdminQuery<IconQuery>,
) -> Result<Response, AdminError> {
    let theme = match query.theme {
        IconTheme::Light => PluginIconTheme::Light,
        IconTheme::Dark => PluginIconTheme::Dark,
    };
    let icon = state
        .admin_services()
        .plugins()
        .icon(&sha256, theme)
        .await
        .map_err(map_admin_service_error)?;
    let content_type =
        HeaderValue::from_str(&icon.content_type).map_err(|_| AdminError::internal())?;
    let mut response = Response::new(Body::from(icon.body));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        // 图标只作为图片加载；SVG 可用内联样式与内嵌图片，但不能执行脚本或请求外部资源
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(response)
}

async fn list<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError> {
    let items: Vec<ArtifactView> = state
        .admin_services()
        .plugins()
        .list()
        .await
        .map_err(map_admin_service_error)?
        .into_iter()
        .map(ArtifactView::new)
        .collect();
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(items)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadQuery {
    sha256: Option<String>,
}

async fn verify_upload<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    bytes: Result<Bytes, BytesRejection>,
) -> Result<impl IntoResponse, AdminError> {
    let bytes = bytes.map_err(|error| {
        AdminError::invalid_request(error.status(), "插件包读取失败或超过大小限制")
    })?;
    let result = state
        .admin_services()
        .plugins()
        .verify_upload(bytes.to_vec().into())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(VerifiedArtifactView::new(result)),
    ))
}

async fn upload<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<UploadQuery>,
    bytes: Result<Bytes, BytesRejection>,
) -> Result<impl IntoResponse, AdminError> {
    let bytes = bytes.map_err(|error| {
        AdminError::invalid_request(error.status(), "插件包读取失败或超过大小限制")
    })?;
    let result = state
        .admin_services()
        .plugins()
        .install_upload(
            bytes.to_vec().into(),
            query.sha256,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(InstallResultView::new(result)),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptArtifact {
    sha256: String,
}

async fn accept<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AcceptArtifact>,
) -> Result<impl IntoResponse, AdminError> {
    let result = state
        .admin_services()
        .plugins()
        .accept_artifact(&request.sha256, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(InstallResultView::new(result)),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArtifact {
    sha256: String,
}

async fn remove<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<DeleteArtifact>,
) -> Result<impl IntoResponse, AdminError> {
    state
        .admin_services()
        .plugins()
        .delete(&request.sha256, &auth.context().mutation_context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(())))
}
