//! xAI 管理端 wire contract。

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::{
    AdminError as AdminServiceError, AdminErrorKind, Revision,
    accounts::{AccountAvailability, AccountRecord},
    provider_credentials::{
        AuthorizationStarted, CompleteAuthorization, CredentialImportResult, CredentialListQuery,
        CredentialListWindow, CredentialMutation, CredentialMutationResult, CredentialPage,
        ImportCredentials, ProviderDocument, StartAuthorization,
    },
};
use gateway_core::{
    engine::credential::{OpaqueProviderData, ProviderAccountId},
    routing::ProviderInstanceId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::admin::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, WireValidationError,
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

    fn into_command(self) -> Result<CredentialListQuery, WireValidationError> {
        self.validate()?;
        Ok(CredentialListQuery {
            provider_instance_id: self
                .provider_instance_id
                .map(ProviderInstanceId::new)
                .transpose()
                .map_err(|_| WireValidationError::new("providerInstanceId"))?,
            availability: None,
            enabled: None,
            window: CredentialListWindow::All,
        })
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

    fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<StartAuthorization, WireValidationError> {
        self.validate()?;
        Ok(StartAuthorization {
            context,
            expected_config_revision: revision(
                self.credential.expected_config_revision,
                "expectedConfigRevision",
            )?,
            provider_instance_id: ProviderInstanceId::new(self.credential.provider_instance_id)
                .map_err(|_| WireValidationError::new("providerInstanceId"))?,
            name: self.credential.name,
            reauthorization: None,
        })
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationStartData {
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

impl From<AccountRecord> for XaiCredentialViewData {
    fn from(account: AccountRecord) -> Self {
        Self {
            id: account.id,
            provider_instance_id: account.provider_instance_id.to_string(),
            name: account.name,
            email: account.email,
            upstream_user_id: account.upstream_user_id,
            upstream_account_id: account.upstream_account_id,
            plan_type: account.plan_type,
            enabled: account.enabled,
            credential_revision: i64::try_from(account.credential_revision.get())
                .unwrap_or(i64::MAX),
            has_refresh_token: account.has_refresh_token,
            availability: availability_name(account.availability).to_owned(),
            availability_reason: account.availability_reason,
            access_token_expires_at: account.access_token_expires_at,
            next_refresh_at: account.next_refresh_at,
            cooldown_until: account.cooldown_until,
            created_at: account.created_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<CredentialPage> for XaiCredentialListData {
    fn from(page: CredentialPage) -> Self {
        Self {
            config_revision: page.config_revision.get(),
            items: page.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<CredentialImportResult> for XaiCredentialImportData {
    fn from(result: CredentialImportResult) -> Self {
        Self::new(
            result.config_revision.get(),
            result
                .credential_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        )
    }
}

impl From<AuthorizationStarted> for AuthorizationStartData {
    fn from(started: AuthorizationStarted) -> Self {
        Self {
            flow_id: started.flow_id,
            authorization_url: started.authorization_url,
            expires_at: started.expires_at,
        }
    }
}

impl From<CredentialMutationResult> for XaiCredentialMutationData {
    fn from(result: CredentialMutationResult) -> Self {
        Self {
            config_revision: result.config_revision.get(),
            credential_id: result.account_id.to_string(),
        }
    }
}

/// 构造固定 GET/POST xAI OAuth 管理路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
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
    S: AdminSessionState + Send + Sync,
{
    let command = query.into_command().map_err(map_wire_error)?;
    let page = state
        .admin_services()
        .xai()
        .list(command)
        .await
        .map_err(map_service_error)?;
    ok(StatusCode::OK, XaiCredentialListData::from(page))
}

async fn import_credentials_document<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<XaiCredentialImportDocumentRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .xai()
        .import_document(command)
        .await
        .map_err(map_service_error)?;
    ok(StatusCode::CREATED, XaiCredentialImportData::from(result))
}

async fn start_authorization_code_flow<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<StartAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let started = state
        .admin_services()
        .xai()
        .start_authorization(command)
        .await
        .map_err(map_service_error)?;
    require_id(&started.flow_id, "flowId").map_err(map_wire_error)?;
    ok(StatusCode::CREATED, AuthorizationStartData::from(started))
}

async fn complete_authorization_code_flow<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<CompleteAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .xai()
        .complete_authorization(command)
        .await
        .map_err(map_service_error)?;
    ok(StatusCode::CREATED, XaiCredentialMutationData::from(result))
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
    Disable,
    Enable,
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
    let service = state.admin_services().xai();
    let result = match action {
        CredentialAction::Disable => service.disable(command).await,
        CredentialAction::Enable => service.enable(command).await,
        CredentialAction::Delete => service.delete(command).await,
    }
    .map_err(map_service_error)?;
    ok(StatusCode::OK, XaiCredentialMutationData::from(result))
}

fn ok<T: Serialize>(
    status: StatusCode,
    data: T,
) -> Result<AdminResponse<AdminEnvelope<T>>, AdminError> {
    Ok(AdminResponse::new(status, AdminEnvelope::ok(data)))
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    AdminError::bad_request(format!("Invalid xAI OAuth field: {}", error.field()))
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminErrorKind::Unauthorized => AdminError::admin_session_required(),
        AdminErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminErrorKind::RateLimited => AdminError::too_many_login_attempts(),
        AdminErrorKind::BadGateway | AdminErrorKind::Unavailable => {
            AdminError::bad_gateway(error.to_string())
        }
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

const fn availability_name(value: AccountAvailability) -> &'static str {
    match value {
        AccountAvailability::Unknown => "unknown",
        AccountAvailability::Ready => "ready",
        AccountAvailability::Cooldown => "cooldown",
        AccountAvailability::QuotaExhausted => "quota_exhausted",
        AccountAvailability::Expired => "expired",
        AccountAvailability::Banned => "banned",
        AccountAvailability::Invalid => "invalid",
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
