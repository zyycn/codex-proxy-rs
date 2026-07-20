//! Codex OAuth credential 管理端 wire contract。

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use gateway_admin::model::{
    AdminError as AdminServiceError, AdminErrorKind, PageSize, Revision,
    accounts::{AccountAvailability, AccountRecord},
    provider_credentials::{
        AuthorizationStarted, CompleteAuthorization, CredentialAvailabilityFilter,
        CredentialCursor, CredentialDetails, CredentialImportResult, CredentialListQuery,
        CredentialListWindow, CredentialMutation, CredentialMutationResult, CredentialPage,
        ImportCredentials, ProviderDocument, ReauthorizationTarget, RotateCredential,
        StartAuthorization,
    },
};
use gateway_core::{
    engine::credential::{OpaqueProviderData, ProviderAccountId},
    routing::ProviderInstanceId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::admin::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, WireValidationError,
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

    fn into_command(self) -> Result<CredentialListQuery, WireValidationError> {
        self.validate()?;
        Ok(CredentialListQuery {
            provider_instance_id: self
                .provider_instance_id
                .map(ProviderInstanceId::new)
                .transpose()
                .map_err(|_| WireValidationError::new("providerInstanceId"))?,
            availability: self
                .availability
                .as_deref()
                .map(parse_availability)
                .transpose()?,
            enabled: self.enabled,
            window: CredentialListWindow::Page {
                cursor: self.cursor.as_deref().map(decode_cursor).transpose()?,
                page_size: PageSize::new(self.limit.unwrap_or(DEFAULT_PAGE_SIZE))
                    .map_err(|_| WireValidationError::new("limit"))?,
            },
        })
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

    fn into_account_id(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.credential_id)
            .map_err(|_| WireValidationError::new("credentialId"))
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

    fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<ImportCredentials, WireValidationError> {
        self.validate()?;
        Ok(ImportCredentials {
            context,
            expected_config_revision: revision(
                self.expected_config_revision,
                "expectedConfigRevision",
            )?,
            provider_instance_id: ProviderInstanceId::new(self.provider_instance_id)
                .map_err(|_| WireValidationError::new("providerInstanceId"))?,
            document: provider_document(self.document, "document")?,
        })
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

    fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<StartAuthorization, WireValidationError> {
        self.validate()?;
        let reauthorization = match (self.credential_id, self.expected_credential_revision) {
            (None, None) => None,
            (Some(account_id), Some(credential_revision)) => Some(ReauthorizationTarget {
                account_id: ProviderAccountId::new(account_id)
                    .map_err(|_| WireValidationError::new("credentialId"))?,
                credential_revision: revision(credential_revision, "expectedCredentialRevision")?,
            }),
            _ => return Err(WireValidationError::new("reauthorization")),
        };
        Ok(StartAuthorization {
            context,
            expected_config_revision: revision(
                self.expected_config_revision,
                "expectedConfigRevision",
            )?,
            provider_instance_id: ProviderInstanceId::new(self.provider_instance_id)
                .map_err(|_| WireValidationError::new("providerInstanceId"))?,
            name: self.name,
            reauthorization,
        })
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
        if !URL_SAFE_NO_PAD
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

    fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<CompleteAuthorization, WireValidationError> {
        self.validate()?;
        Ok(CompleteAuthorization {
            context,
            flow_id: self.flow_id,
            callback_url: self.callback_url,
        })
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

    fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<RotateCredential, WireValidationError> {
        self.validate()?;
        let mut material = Map::new();
        material.insert("access_token".to_owned(), Value::String(self.access_token));
        material.insert(
            "refresh_token".to_owned(),
            self.refresh_token.map_or(Value::Null, Value::String),
        );
        Ok(RotateCredential {
            mutation: CredentialMutation {
                context,
                expected_config_revision: revision(
                    self.expected_config_revision,
                    "expectedConfigRevision",
                )?,
                account_id: ProviderAccountId::new(self.credential_id)
                    .map_err(|_| WireValidationError::new("credentialId"))?,
            },
            expected_credential_revision: revision(
                self.expected_credential_revision,
                "expectedCredentialRevision",
            )?,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(material)),
        })
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

    fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<CredentialMutation, WireValidationError> {
        self.validate()?;
        Ok(CredentialMutation {
            context,
            expected_config_revision: revision(
                self.expected_config_revision,
                "expectedConfigRevision",
            )?,
            account_id: ProviderAccountId::new(self.credential_id)
                .map_err(|_| WireValidationError::new("credentialId"))?,
        })
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

impl From<AccountRecord> for CodexCredentialView {
    fn from(account: AccountRecord) -> Self {
        Self {
            id: account.id,
            provider_instance_id: account.provider_instance_id.to_string(),
            name: account.name,
            email: account.email,
            upstream_user_id: account.upstream_user_id,
            upstream_account_id: account.upstream_account_id,
            enabled: account.enabled,
            credential_revision: i64::try_from(account.credential_revision.get())
                .unwrap_or(i64::MAX),
            has_refresh_token: account.has_refresh_token,
            availability: codex_availability(account.availability).to_owned(),
            availability_reason: account.availability_reason,
            plan_type: account.plan_type,
            access_token_expires_at: account.access_token_expires_at,
            next_refresh_at: account.next_refresh_at,
            cooldown_until: account.cooldown_until,
            created_at: account.created_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<CredentialDetails> for CodexCredentialDetailsData {
    fn from(details: CredentialDetails) -> Self {
        Self {
            config_revision: details.config_revision.get(),
            credential: details.credential.into(),
        }
    }
}

impl From<CredentialImportResult> for CodexCredentialsDocumentImportData {
    fn from(result: CredentialImportResult) -> Self {
        Self {
            config_revision: result.config_revision.get(),
            credential_ids: result
                .credential_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        }
    }
}

impl From<AuthorizationStarted> for CodexOAuthAuthorizationStartedData {
    fn from(started: AuthorizationStarted) -> Self {
        Self {
            flow_id: started.flow_id,
            authorization_url: started.authorization_url,
            expires_at: started.expires_at,
        }
    }
}

impl From<CredentialMutationResult> for CodexCredentialMutationData {
    fn from(result: CredentialMutationResult) -> Self {
        Self {
            config_revision: result.config_revision.get(),
            credential_id: result.account_id.to_string(),
        }
    }
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

/// 构造固定 GET/POST Codex credential 管理路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
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
    S: AdminSessionState + Send + Sync,
{
    let command = query.into_command().map_err(map_wire_error)?;
    let page = state
        .admin_services()
        .openai()
        .list(command)
        .await
        .map_err(map_service_error)?;
    ok(StatusCode::OK, credential_list_data(page)?)
}

async fn credential_details<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<CredentialDetailsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_account_id().map_err(map_wire_error)?;
    let details = state
        .admin_services()
        .openai()
        .details(&account_id)
        .await
        .map_err(map_service_error)?;
    ok(StatusCode::OK, CodexCredentialDetailsData::from(details))
}

async fn import_credentials_document<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<ImportCredentialsDocumentRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .openai()
        .import_document(command)
        .await
        .map_err(map_service_error)?;
    ok(
        StatusCode::CREATED,
        CodexCredentialsDocumentImportData::from(result),
    )
}

async fn start_oauth_authorization<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<StartOAuthAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .openai()
        .start_authorization(command)
        .await
        .map_err(map_service_error)?;
    ok(
        StatusCode::CREATED,
        CodexOAuthAuthorizationStartedData::from(result),
    )
}

async fn complete_oauth_authorization<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CompleteOAuthAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .openai()
        .complete_authorization(command)
        .await
        .map_err(map_service_error)?;
    ok(
        StatusCode::CREATED,
        CodexCredentialMutationData::from(result),
    )
}

async fn rotate_credential<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<RotateCredentialRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .openai()
        .rotate(command)
        .await
        .map_err(map_service_error)?;
    ok(StatusCode::OK, credential_rotation_data(result)?)
}

async fn enable_credential<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CredentialMutationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    mutate_credential(auth, state, request, CredentialAction::Enable).await
}

async fn disable_credential<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CredentialMutationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    mutate_credential(auth, state, request, CredentialAction::Disable).await
}

async fn delete_credential<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CredentialMutationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    mutate_credential(auth, state, request, CredentialAction::Delete).await
}

#[derive(Clone, Copy)]
enum CredentialAction {
    Enable,
    Disable,
    Delete,
}

async fn mutate_credential<S>(
    auth: AdminAuth,
    state: S,
    request: CredentialMutationRequest,
    action: CredentialAction,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let service = state.admin_services().openai();
    let result = match action {
        CredentialAction::Enable => service.enable(command).await,
        CredentialAction::Disable => service.disable(command).await,
        CredentialAction::Delete => service.delete(command).await,
    }
    .map_err(map_service_error)?;
    ok(StatusCode::OK, CodexCredentialMutationData::from(result))
}

fn ok<T: Serialize>(
    status: StatusCode,
    data: T,
) -> Result<AdminResponse<AdminEnvelope<T>>, AdminError> {
    Ok(AdminResponse::new(status, AdminEnvelope::ok(data)))
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("Invalid Codex credential field: {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminErrorKind::Unauthorized => AdminError::admin_session_required(),
        AdminErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminErrorKind::RateLimited => AdminError::too_many_login_attempts(),
        AdminErrorKind::BadGateway => AdminError::bad_gateway(error.to_string()),
        AdminErrorKind::Unavailable => AdminError::service_unavailable(error.to_string()),
        AdminErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}

fn revision(value: u64, field: &'static str) -> Result<Revision, WireValidationError> {
    Revision::new(value).map_err(|_| WireValidationError::new(field))
}

fn provider_document(
    value: Value,
    field: &'static str,
) -> Result<ProviderDocument, WireValidationError> {
    match value {
        Value::Object(document) => Ok(ProviderDocument::new(OpaqueProviderData::new(document))),
        _ => Err(WireValidationError::new(field)),
    }
}

fn parse_availability(value: &str) -> Result<CredentialAvailabilityFilter, WireValidationError> {
    match value {
        "unknown" => Ok(CredentialAvailabilityFilter::Exact(
            AccountAvailability::Unknown,
        )),
        "ready" => Ok(CredentialAvailabilityFilter::Exact(
            AccountAvailability::Ready,
        )),
        "cooldown" => Ok(CredentialAvailabilityFilter::Exact(
            AccountAvailability::Cooldown,
        )),
        "exhausted" => Ok(CredentialAvailabilityFilter::Exact(
            AccountAvailability::QuotaExhausted,
        )),
        "invalid" => Ok(CredentialAvailabilityFilter::AnyOf(vec![
            AccountAvailability::Expired,
            AccountAvailability::Banned,
            AccountAvailability::Invalid,
        ])),
        _ => Err(WireValidationError::new("availability")),
    }
}

fn decode_cursor(value: &str) -> Result<CredentialCursor, WireValidationError> {
    let encoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| WireValidationError::new("cursor"))?;
    let wire: CredentialCursorWire =
        serde_json::from_slice(&encoded).map_err(|_| WireValidationError::new("cursor"))?;
    require_id(&wire.credential_id, "cursor")?;
    Ok(CredentialCursor {
        created_at: wire.created_at,
        account_id: ProviderAccountId::new(wire.credential_id)
            .map_err(|_| WireValidationError::new("cursor"))?,
    })
}

fn encode_cursor(cursor: &CredentialCursor) -> Result<String, AdminError> {
    let encoded = serde_json::to_vec(&CredentialCursorWire {
        created_at: cursor.created_at,
        credential_id: cursor.account_id.to_string(),
    })
    .map_err(|_| AdminError::internal("Failed to encode credential cursor"))?;
    Ok(URL_SAFE_NO_PAD.encode(encoded))
}

fn credential_list_data(page: CredentialPage) -> Result<CodexCredentialListData, AdminError> {
    Ok(CodexCredentialListData {
        config_revision: page.config_revision.get(),
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor.as_ref().map(encode_cursor).transpose()?,
    })
}

fn credential_rotation_data(
    result: CredentialMutationResult,
) -> Result<CodexCredentialRotationData, AdminError> {
    let credential_revision = result
        .credential_revision
        .ok_or_else(|| AdminError::internal("Credential revision is missing"))?;
    Ok(CodexCredentialRotationData {
        credential_id: result.account_id.to_string(),
        credential_revision: i64::try_from(credential_revision.get()).unwrap_or(i64::MAX),
    })
}

const fn codex_availability(value: AccountAvailability) -> &'static str {
    match value {
        AccountAvailability::Unknown => "unknown",
        AccountAvailability::Ready => "ready",
        AccountAvailability::Cooldown => "cooldown",
        AccountAvailability::QuotaExhausted => "exhausted",
        AccountAvailability::Expired
        | AccountAvailability::Banned
        | AccountAvailability::Invalid => "invalid",
    }
}
