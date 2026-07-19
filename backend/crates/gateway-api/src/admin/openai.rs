//! Codex OAuth credential 管理端 wire contract。

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

pub const DEFAULT_PAGE_SIZE: u16 = 50;
pub const MAX_PAGE_SIZE: u16 = 200;
const MAX_ID_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 512;
const MAX_CURSOR_BYTES: usize = 1_024;
const MAX_IMPORT_DOCUMENT_BYTES: usize = 64 * 1_024 * 1_024;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1_024;
const MAX_REFRESH_TOKEN_BYTES: usize = 64 * 1_024;
const MAX_CALLBACK_URL_BYTES: usize = 16 * 1_024;

/// Codex credential 列表查询。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListCredentialsQuery {
    pub provider_instance_id: Option<String>,
    pub availability: Option<String>,
    pub enabled: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl ListCredentialsQuery {
    /// 校验筛选与游标边界。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if let Some(provider_instance_id) = self.provider_instance_id.as_deref() {
            require_id(provider_instance_id, "providerInstanceId")?;
        }
        if self.availability.as_deref().is_some_and(|value| {
            !matches!(
                value,
                "unknown" | "ready" | "cooldown" | "exhausted" | "invalid"
            )
        }) {
            return Err(WireValidationError::new("availability"));
        }
        if self
            .cursor
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.len() > MAX_CURSOR_BYTES)
        {
            return Err(WireValidationError::new("cursor"));
        }
        if self
            .limit
            .is_some_and(|limit| limit == 0 || limit > MAX_PAGE_SIZE)
        {
            return Err(WireValidationError::new("limit"));
        }
        Ok(())
    }
}

/// Codex credential 详情查询。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialDetailsQuery {
    pub credential_id: String,
}

impl CredentialDetailsQuery {
    /// 校验 credential ID。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.credential_id, "credentialId")
    }
}

/// OpenAI Provider-owned 账号导入文档。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportCredentialsDocumentRequest {
    pub expected_config_revision: u64,
    pub provider_instance_id: String,
    pub document: Value,
}

impl ImportCredentialsDocumentRequest {
    /// Wire 只约束文档大小；结构识别与 token 验证完全归 Provider。
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

/// 启动 Codex Authorization Code + PKCE；可创建新账号或绑定既有账号重新授权。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartOAuthAuthorizationRequest {
    pub expected_config_revision: u64,
    pub provider_instance_id: String,
    pub name: String,
    pub credential_id: Option<String>,
    pub expected_credential_revision: Option<u64>,
}

impl StartOAuthAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_positive(self.expected_config_revision, "expectedConfigRevision")?;
        require_id(&self.provider_instance_id, "providerInstanceId")?;
        require_text(&self.name, MAX_NAME_BYTES, "name")?;
        match (
            self.credential_id.as_deref(),
            self.expected_credential_revision,
        ) {
            (None, None) => {}
            (Some(credential_id), Some(revision)) => {
                require_id(credential_id, "credentialId")?;
                require_positive(revision, "expectedCredentialRevision")?;
            }
            _ => return Err(WireValidationError::new("reauthorization")),
        }
        Ok(())
    }
}

/// 完成 Codex Authorization Code flow；flow ID 与 callback 都放在 POST body。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteOAuthAuthorizationRequest {
    pub flow_id: String,
    pub callback_url: String,
}

impl CompleteOAuthAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        use base64::Engine as _;

        if !base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.flow_id)
            .is_ok_and(|decoded| decoded.len() == 32)
        {
            return Err(WireValidationError::new("flowId"));
        }
        if self.callback_url.is_empty()
            || self.callback_url.len() > MAX_CALLBACK_URL_BYTES
            || self.callback_url.chars().any(char::is_control)
        {
            return Err(WireValidationError::new("callbackUrl"));
        }
        Ok(())
    }
}

/// Codex OAuth token 轮换请求。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotateCredentialRequest {
    pub credential_id: String,
    pub expected_config_revision: u64,
    pub expected_credential_revision: u64,
    pub access_token: String,
    pub refresh_token: Option<String>,
}

impl RotateCredentialRequest {
    /// 校验 ID、revision 与 OAuth material 的 wire 形状。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.credential_id, "credentialId")?;
        require_positive(self.expected_config_revision, "expectedConfigRevision")?;
        require_positive(
            self.expected_credential_revision,
            "expectedCredentialRevision",
        )?;
        validate_oauth_material(&self.access_token, self.refresh_token.as_deref())
    }
}

/// Codex credential 生命周期 mutation 请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialMutationRequest {
    pub credential_id: String,
    pub expected_config_revision: u64,
}

impl CredentialMutationRequest {
    /// 校验 ID 与配置 revision。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_id(&self.credential_id, "credentialId")?;
        require_positive(self.expected_config_revision, "expectedConfigRevision")
    }
}

/// 列表游标的 JSON 形状；HTTP 组合层负责 base64url 编解码。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialCursorWire {
    pub created_at: DateTime<Utc>,
    pub credential_id: String,
}

/// Admin API 可安全返回的 Codex credential 视图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredentialView {
    pub id: String,
    pub provider_instance_id: String,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: String,
    pub upstream_account_id: Option<String>,
    pub enabled: bool,
    pub credential_revision: i64,
    pub has_refresh_token: bool,
    pub availability: String,
    pub availability_reason: Option<String>,
    pub plan_type: Option<String>,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Codex credential 列表响应数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredentialListData {
    pub config_revision: u64,
    pub items: Vec<CodexCredentialView>,
    pub next_cursor: Option<String>,
}

/// Codex credential 详情响应数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredentialDetailsData {
    pub config_revision: u64,
    pub credential: CodexCredentialView,
}

/// 已提交的结构配置 mutation 响应数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredentialMutationData {
    pub config_revision: u64,
    pub credential_id: String,
}

/// Provider 正式文档一次原子导入的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredentialsDocumentImportData {
    pub config_revision: u64,
    pub credential_ids: Vec<String>,
}

/// Authorization Code + PKCE 启动响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexOAuthAuthorizationStartedData {
    pub flow_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

/// 已提交的 token 轮换响应数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredentialRotationData {
    pub credential_id: String,
    pub credential_revision: i64,
}

fn validate_oauth_material(
    access_token: &str,
    refresh_token: Option<&str>,
) -> Result<(), WireValidationError> {
    if access_token.len() > MAX_ACCESS_TOKEN_BYTES
        || !valid_visible_ascii(access_token)
        || !valid_compact_jwt_shape(access_token)
    {
        return Err(WireValidationError::new("accessToken"));
    }
    if refresh_token.is_some_and(|token| {
        token.len() > MAX_REFRESH_TOKEN_BYTES
            || !valid_visible_ascii(token)
            || token == access_token
    }) {
        return Err(WireValidationError::new("refreshToken"));
    }
    Ok(())
}

fn valid_compact_jwt_shape(value: &str) -> bool {
    let mut segments = value.split('.');
    matches!(
        (segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty()
    )
}

fn valid_visible_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn require_id(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    require_text(value, MAX_ID_BYTES, field)?;
    if value.starts_with("__") {
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

/// Codex OAuth credential 管理应用端口。
#[async_trait]
pub trait CodexAdminService: Send + Sync {
    async fn list(
        &self,
        query: ListCredentialsQuery,
    ) -> Result<CodexCredentialListData, AdminServiceError>;
    async fn details(
        &self,
        credential_id: String,
    ) -> Result<CodexCredentialDetailsData, AdminServiceError>;
    async fn import_document(
        &self,
        context: &AdminRequestContext,
        request: ImportCredentialsDocumentRequest,
    ) -> Result<CodexCredentialsDocumentImportData, AdminServiceError>;
    async fn start_authorization(
        &self,
        context: &AdminRequestContext,
        request: StartOAuthAuthorizationRequest,
    ) -> Result<CodexOAuthAuthorizationStartedData, AdminServiceError>;
    async fn complete_authorization(
        &self,
        context: &AdminRequestContext,
        request: CompleteOAuthAuthorizationRequest,
    ) -> Result<CodexCredentialMutationData, AdminServiceError>;
    async fn rotate(
        &self,
        context: &AdminRequestContext,
        request: RotateCredentialRequest,
    ) -> Result<CodexCredentialRotationData, AdminServiceError>;
    async fn enable(
        &self,
        context: &AdminRequestContext,
        request: CredentialMutationRequest,
    ) -> Result<CodexCredentialMutationData, AdminServiceError>;
    async fn disable(
        &self,
        context: &AdminRequestContext,
        request: CredentialMutationRequest,
    ) -> Result<CodexCredentialMutationData, AdminServiceError>;
    async fn delete(
        &self,
        context: &AdminRequestContext,
        request: CredentialMutationRequest,
    ) -> Result<CodexCredentialMutationData, AdminServiceError>;
}

/// Codex HTTP module 所需最小 state。
pub trait CodexAdminState: AdminSessionState {
    fn codex_admin_service(&self) -> &dyn CodexAdminService;
}

/// 构造固定 GET/POST Codex credential 管理路由。
pub fn router<S>() -> Router<S>
where
    S: CodexAdminState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/openai/credentials", get(list_credentials::<S>))
        .route("/api/admin/openai/credential", get(credential_details::<S>))
        .route(
            "/api/admin/openai/credentials/import-document",
            post(import_credentials_document::<S>),
        )
        .route(
            "/api/admin/openai/oauth/authorization/start",
            post(start_oauth_authorization::<S>),
        )
        .route(
            "/api/admin/openai/oauth/authorization/complete",
            post(complete_oauth_authorization::<S>),
        )
        .route(
            "/api/admin/openai/credentials/rotate",
            post(rotate_credential::<S>),
        )
        .route(
            "/api/admin/openai/credentials/enable",
            post(enable_credential::<S>),
        )
        .route(
            "/api/admin/openai/credentials/disable",
            post(disable_credential::<S>),
        )
        .route(
            "/api/admin/openai/credentials/delete",
            post(delete_credential::<S>),
        )
}

async fn list_credentials<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<ListCredentialsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: CodexAdminState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    ok(
        StatusCode::OK,
        state.codex_admin_service().list(query).await,
    )
}

async fn credential_details<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<CredentialDetailsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: CodexAdminState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    ok(
        StatusCode::OK,
        state
            .codex_admin_service()
            .details(query.credential_id)
            .await,
    )
}

macro_rules! codex_post_handler {
    ($name:ident, $request:ty, $method:ident, $status:expr) => {
        async fn $name<S>(
            auth: AdminAuth,
            State(state): State<S>,
            Json(request): Json<$request>,
        ) -> Result<impl IntoResponse, AdminError>
        where
            S: CodexAdminState + Send + Sync,
        {
            request.validate().map_err(map_wire_error)?;
            ok(
                $status,
                state
                    .codex_admin_service()
                    .$method(auth.context(), request)
                    .await,
            )
        }
    };
}

codex_post_handler!(
    import_credentials_document,
    ImportCredentialsDocumentRequest,
    import_document,
    StatusCode::CREATED
);
codex_post_handler!(
    start_oauth_authorization,
    StartOAuthAuthorizationRequest,
    start_authorization,
    StatusCode::CREATED
);
codex_post_handler!(
    complete_oauth_authorization,
    CompleteOAuthAuthorizationRequest,
    complete_authorization,
    StatusCode::CREATED
);
codex_post_handler!(
    rotate_credential,
    RotateCredentialRequest,
    rotate,
    StatusCode::OK
);
codex_post_handler!(
    enable_credential,
    CredentialMutationRequest,
    enable,
    StatusCode::OK
);
codex_post_handler!(
    disable_credential,
    CredentialMutationRequest,
    disable,
    StatusCode::OK
);
codex_post_handler!(
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
    AdminError::bad_request(format!("Invalid Codex credential field: {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminServiceErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminServiceErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminServiceErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminServiceErrorKind::Unavailable => AdminError::service_unavailable(error.to_string()),
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
