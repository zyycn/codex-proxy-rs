//! xAI 管理端 wire contract。

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::admin::{
    AdminAuth, AdminEnvelope, AdminError, AdminRequestContext, AdminResponse, AdminServiceError,
    AdminServiceErrorKind, AdminSessionState, WireValidationError,
};

const MAX_IMPORT_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

/// xAI Provider-owned 正式导入文档；API 不解释文档内部字段。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct XaiCredentialImportDocumentRequest {
    pub expected_config_revision: u64,
    pub provider_instance_id: String,
    pub document: Value,
}

impl XaiCredentialImportDocumentRequest {
    /// 只校验公共 revision、instance 和请求体边界；Provider 负责全部格式语义。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_positive(self.expected_config_revision, "expectedConfigRevision")?;
        require_id(&self.provider_instance_id, "providerInstanceId")?;
        if !self.document.is_object()
            || serde_json::to_vec(&self.document)
                .map_or(true, |encoded| encoded.len() > MAX_IMPORT_DOCUMENT_BYTES)
        {
            return Err(WireValidationError::new("document"));
        }
        Ok(())
    }
}

/// 批量导入成功响应；不包含任何 token、subject、邮箱或 client ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiCredentialImportData {
    pub config_revision: u64,
    pub imported_count: usize,
    pub credential_ids: Vec<String>,
}

impl XaiCredentialImportData {
    #[must_use]
    pub fn new(config_revision: u64, credential_ids: Vec<String>) -> Self {
        Self {
            config_revision,
            imported_count: credential_ids.len(),
            credential_ids,
        }
    }
}

const MAX_ID_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 512;
const MAX_CALLBACK_FIELD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListCredentialsQuery {
    pub provider_instance_id: Option<String>,
}

impl ListCredentialsQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if let Some(id) = self.provider_instance_id.as_deref() {
            require_id(id, "providerInstanceId")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewCredentialRequest {
    pub expected_config_revision: u64,
    pub provider_instance_id: String,
    pub name: String,
}

impl NewCredentialRequest {
    fn validate(&self) -> Result<(), WireValidationError> {
        require_positive(self.expected_config_revision, "expectedConfigRevision")?;
        require_id(&self.provider_instance_id, "providerInstanceId")?;
        require_text(&self.name, MAX_NAME_BYTES, "name")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartAuthorizationRequest {
    #[serde(flatten)]
    pub credential: NewCredentialRequest,
}

impl StartAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        self.credential.validate()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteAuthorizationRequest {
    pub flow_id: String,
    pub callback_url: String,
}

impl CompleteAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.flow_id, "flowId")?;
        require_text(&self.callback_url, MAX_CALLBACK_FIELD_BYTES, "callbackUrl")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialMutationRequest {
    pub credential_id: String,
    pub expected_config_revision: u64,
}

impl CredentialMutationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.credential_id, "credentialId")?;
        require_positive(self.expected_config_revision, "expectedConfigRevision")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationStartData {
    pub flow_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

/// Authorization Code flow 启动的应用端结果。
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationStartResult {
    pub flow_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

/// xAI OAuth credential 的安全管理视图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiCredentialViewData {
    pub id: String,
    pub provider_instance_id: String,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: String,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub enabled: bool,
    pub credential_revision: i64,
    pub has_refresh_token: bool,
    pub availability: String,
    pub availability_reason: Option<String>,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// xAI OAuth credential 列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiCredentialListData {
    pub config_revision: u64,
    pub items: Vec<XaiCredentialViewData>,
}

/// Credential 生命周期 mutation 响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiCredentialMutationData {
    pub config_revision: u64,
    pub credential_id: String,
}

/// xAI OAuth credential 管理应用端口。
#[async_trait]
pub trait XaiAdminService: Send + Sync {
    async fn list(
        &self,
        query: ListCredentialsQuery,
    ) -> Result<XaiCredentialListData, AdminServiceError>;
    async fn import_document(
        &self,
        context: &AdminRequestContext,
        request: XaiCredentialImportDocumentRequest,
    ) -> Result<XaiCredentialImportData, AdminServiceError>;
    async fn start_authorization(
        &self,
        context: &AdminRequestContext,
        request: StartAuthorizationRequest,
    ) -> Result<AuthorizationStartResult, AdminServiceError>;
    async fn complete_authorization(
        &self,
        context: &AdminRequestContext,
        request: CompleteAuthorizationRequest,
    ) -> Result<XaiCredentialMutationData, AdminServiceError>;
    async fn disable(
        &self,
        context: &AdminRequestContext,
        request: CredentialMutationRequest,
    ) -> Result<XaiCredentialMutationData, AdminServiceError>;
    async fn enable(
        &self,
        context: &AdminRequestContext,
        request: CredentialMutationRequest,
    ) -> Result<XaiCredentialMutationData, AdminServiceError>;
    async fn delete(
        &self,
        context: &AdminRequestContext,
        request: CredentialMutationRequest,
    ) -> Result<XaiCredentialMutationData, AdminServiceError>;
}

/// xAI HTTP module 所需最小 state。
pub trait XaiAdminState: AdminSessionState {
    fn xai_admin_service(&self) -> &dyn XaiAdminService;
}

/// 构造固定 GET/POST xAI OAuth 管理路由。
pub fn router<S>() -> Router<S>
where
    S: XaiAdminState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/xai/credentials", get(list_credentials::<S>))
        .route(
            "/api/admin/xai/credentials/import-document",
            post(import_credentials_document::<S>),
        )
        .route(
            "/api/admin/xai/oauth/authorization/start",
            post(start_authorization_code_flow::<S>),
        )
        .route(
            "/api/admin/xai/oauth/authorization/complete",
            post(complete_authorization_code_flow::<S>),
        )
        .route(
            "/api/admin/xai/credentials/disable",
            post(disable_credential::<S>),
        )
        .route(
            "/api/admin/xai/credentials/enable",
            post(enable_credential::<S>),
        )
        .route(
            "/api/admin/xai/credentials/delete",
            post(delete_credential::<S>),
        )
}

async fn list_credentials<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<ListCredentialsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: XaiAdminState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    ok(StatusCode::OK, state.xai_admin_service().list(query).await)
}

macro_rules! xai_post_handler {
    ($name:ident, $request:ty, $method:ident, $status:expr) => {
        async fn $name<S>(
            auth: AdminAuth,
            State(state): State<S>,
            Json(request): Json<$request>,
        ) -> Result<impl IntoResponse, AdminError>
        where
            S: XaiAdminState + Send + Sync,
        {
            request.validate().map_err(map_wire_error)?;
            ok(
                $status,
                state
                    .xai_admin_service()
                    .$method(auth.context(), request)
                    .await,
            )
        }
    };
}

xai_post_handler!(
    import_credentials_document,
    XaiCredentialImportDocumentRequest,
    import_document,
    StatusCode::CREATED
);
async fn start_authorization_code_flow<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<StartAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: XaiAdminState + Send + Sync,
{
    request.validate().map_err(map_wire_error)?;
    let started = state
        .xai_admin_service()
        .start_authorization(auth.context(), request)
        .await
        .map_err(map_service_error)?;
    require_id(&started.flow_id, "flowId").map_err(map_wire_error)?;
    let data = AuthorizationStartData {
        authorization_url: started.authorization_url,
        flow_id: started.flow_id,
        expires_at: started.expires_at,
    };
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(data),
    ))
}
xai_post_handler!(
    complete_authorization_code_flow,
    CompleteAuthorizationRequest,
    complete_authorization,
    StatusCode::CREATED
);
xai_post_handler!(
    disable_credential,
    CredentialMutationRequest,
    disable,
    StatusCode::OK
);
xai_post_handler!(
    enable_credential,
    CredentialMutationRequest,
    enable,
    StatusCode::OK
);
xai_post_handler!(
    delete_credential,
    CredentialMutationRequest,
    delete,
    StatusCode::OK
);

fn ok<T: Serialize>(
    status: StatusCode,
    result: Result<T, AdminServiceError>,
) -> Result<AdminResponse<AdminEnvelope<T>>, AdminError> {
    let data = result.map_err(map_service_error)?;
    Ok(AdminResponse::new(status, AdminEnvelope::ok(data)))
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("Invalid xAI OAuth field: {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminServiceErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminServiceErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminServiceErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminServiceErrorKind::Unavailable => AdminError::bad_gateway(error.to_string()),
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}

fn require_id(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    require_text(value, MAX_ID_BYTES, field)?;
    if value.starts_with("__")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn require_text(
    value: &str,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), WireValidationError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn require_positive(value: u64, field: &'static str) -> Result<(), WireValidationError> {
    if value == 0 || i64::try_from(value).is_err() {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}
