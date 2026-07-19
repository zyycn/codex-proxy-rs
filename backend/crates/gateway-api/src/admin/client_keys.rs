//! Client API Key 管理 wire contract。

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminRequestContext, AdminResponse, AdminServiceError,
    AdminServiceErrorKind, AdminSessionState, WireValidationError,
};

const MAX_CURSOR_BYTES: usize = 512;
const MAX_SEARCH_BYTES: usize = 256;

/// Client Key 列表查询。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListClientKeysQuery {
    cursor: Option<String>,
    limit: Option<u16>,
    search: Option<String>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

impl ListClientKeysQuery {
    /// 校验游标与页大小的 wire 边界并取出字段。
    pub fn into_parts(self) -> Result<ListClientKeysFields, WireValidationError> {
        if self
            .cursor
            .as_deref()
            .is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES)
        {
            return Err(WireValidationError::new("cursor"));
        }
        if self.limit == Some(0) {
            return Err(WireValidationError::new("limit"));
        }
        let search = self.search.map(|search| search.trim().to_owned());
        if search.as_deref().is_some_and(|search| {
            search.len() > MAX_SEARCH_BYTES
                || search.chars().any(char::is_control)
                || contains_client_key_material(search)
        }) {
            return Err(WireValidationError::new("search"));
        }
        let sort = ClientKeySort::parse(
            self.sort_by.as_deref().unwrap_or("createdAt"),
            self.sort_direction.as_deref().unwrap_or("desc"),
        )?;
        Ok(ListClientKeysFields {
            cursor: self.cursor,
            limit: self.limit,
            search: search.filter(|search| !search.is_empty()),
            sort,
        })
    }
}

/// Client Key 数据库排序字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientKeySortField {
    Name,
    Enabled,
    CreatedAt,
    LastUsedAt,
}

/// Client Key 数据库排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKeySortDirection {
    Asc,
    Desc,
}

/// 已校验且会写入自描述游标的排序组合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientKeySort {
    pub field: ClientKeySortField,
    pub direction: ClientKeySortDirection,
}

impl ClientKeySort {
    fn parse(field: &str, direction: &str) -> Result<Self, WireValidationError> {
        let field = match field {
            "name" => ClientKeySortField::Name,
            "enabled" => ClientKeySortField::Enabled,
            "createdAt" => ClientKeySortField::CreatedAt,
            "lastUsedAt" => ClientKeySortField::LastUsedAt,
            _ => return Err(WireValidationError::new("sortBy")),
        };
        let direction = match direction {
            "asc" => ClientKeySortDirection::Asc,
            "desc" => ClientKeySortDirection::Desc,
            _ => return Err(WireValidationError::new("sortDirection")),
        };
        Ok(Self { field, direction })
    }
}

/// 已校验的 Client Key 列表查询字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListClientKeysFields {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
    pub search: Option<String>,
    pub sort: ClientKeySort,
}

/// 创建 Client Key 请求。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateClientKeyRequest {
    expected_config_revision: u64,
    name: String,
    label: Option<String>,
    provider_kind: String,
    max_concurrency: u64,
    requests_per_minute: u64,
    tokens_per_minute: u64,
}

impl CreateClientKeyRequest {
    /// 校验请求并转换为应用层可消费字段。
    pub fn into_fields(self) -> Result<CreateClientKeyFields, WireValidationError> {
        validate_revision(self.expected_config_revision)?;
        validate_required_text(&self.name, "name")?;
        validate_optional_text(self.label.as_deref(), "label")?;
        validate_provider_kind(&self.provider_kind)?;
        validate_limit(self.max_concurrency, "maxConcurrency")?;
        validate_limit(self.requests_per_minute, "requestsPerMinute")?;
        validate_limit(self.tokens_per_minute, "tokensPerMinute")?;
        Ok(CreateClientKeyFields {
            expected_config_revision: self.expected_config_revision,
            name: self.name,
            label: self.label,
            provider_kind: self.provider_kind,
            max_concurrency: self.max_concurrency,
            requests_per_minute: self.requests_per_minute,
            tokens_per_minute: self.tokens_per_minute,
        })
    }
}

/// 已校验的 Client Key 创建字段。
#[derive(Debug, Clone, PartialEq)]
pub struct CreateClientKeyFields {
    pub expected_config_revision: u64,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

/// 更新 Client Key 请求。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateClientKeyRequest {
    id: String,
    expected_config_revision: u64,
    name: String,
    label: Option<String>,
    provider_kind: String,
    max_concurrency: u64,
    requests_per_minute: u64,
    tokens_per_minute: u64,
}

impl UpdateClientKeyRequest {
    /// 校验请求并转换为应用层可消费字段。
    pub fn into_fields(self) -> Result<UpdateClientKeyFields, WireValidationError> {
        validate_required_text(&self.id, "id")?;
        validate_revision(self.expected_config_revision)?;
        validate_required_text(&self.name, "name")?;
        validate_optional_text(self.label.as_deref(), "label")?;
        validate_provider_kind(&self.provider_kind)?;
        validate_limit(self.max_concurrency, "maxConcurrency")?;
        validate_limit(self.requests_per_minute, "requestsPerMinute")?;
        validate_limit(self.tokens_per_minute, "tokensPerMinute")?;
        Ok(UpdateClientKeyFields {
            id: self.id,
            expected_config_revision: self.expected_config_revision,
            name: self.name,
            label: self.label,
            provider_kind: self.provider_kind,
            max_concurrency: self.max_concurrency,
            requests_per_minute: self.requests_per_minute,
            tokens_per_minute: self.tokens_per_minute,
        })
    }
}

/// 已校验的 Client Key 更新字段。
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateClientKeyFields {
    pub id: String,
    pub expected_config_revision: u64,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

/// 只携带 ID 与配置 revision 的 Client Key mutation 请求。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientKeyRevisionRequest {
    id: String,
    expected_config_revision: u64,
}

/// 读取一次完整 Key 的 ID query。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientKeyIdQuery {
    id: String,
}

impl ClientKeyIdQuery {
    pub fn into_id(self) -> Result<String, WireValidationError> {
        validate_required_text(&self.id, "id")?;
        Ok(self.id)
    }
}

impl ClientKeyRevisionRequest {
    /// 校验请求并取出 ID 与 revision。
    pub fn into_parts(self) -> Result<(String, u64), WireValidationError> {
        validate_required_text(&self.id, "id")?;
        validate_revision(self.expected_config_revision)?;
        Ok((self.id, self.expected_config_revision))
    }
}

/// 不含完整 Key 的管理端安全视图。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientKeyView {
    id: String,
    name: String,
    label: Option<String>,
    provider_kind: String,
    prefix: String,
    enabled: bool,
    max_concurrency: u64,
    requests_per_minute: u64,
    tokens_per_minute: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

impl ClientKeyView {
    /// 从应用层已经读取的安全字段构造视图。
    #[must_use]
    pub fn new(fields: ClientKeyViewFields) -> Self {
        Self {
            id: fields.id,
            name: fields.name,
            label: fields.label,
            provider_kind: fields.provider_kind,
            prefix: fields.prefix,
            enabled: fields.enabled,
            max_concurrency: fields.max_concurrency,
            requests_per_minute: fields.requests_per_minute,
            tokens_per_minute: fields.tokens_per_minute,
            created_at: fields.created_at,
            updated_at: fields.updated_at,
            last_used_at: fields.last_used_at,
        }
    }
}

/// Client Key 安全视图的应用层输入字段。
#[derive(Debug, Clone, PartialEq)]
pub struct ClientKeyViewFields {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: String,
    pub prefix: String,
    pub enabled: bool,
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Client Key 列表响应数据。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientKeyListData {
    config_revision: u64,
    items: Vec<ClientKeyView>,
    next_cursor: Option<String>,
    total: u64,
}

impl ClientKeyListData {
    /// 构造 Client Key 列表响应。
    #[must_use]
    pub fn new(
        config_revision: u64,
        items: Vec<ClientKeyView>,
        next_cursor: Option<String>,
        total: u64,
    ) -> Self {
        Self {
            config_revision,
            items,
            next_cursor,
            total,
        }
    }
}

/// Client Key 创建响应；完整值只允许出现在本次序列化结果中。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedClientKeyData {
    config_revision: u64,
    id: String,
    prefix: String,
    plaintext_key: String,
}

impl CreatedClientKeyData {
    /// 构造一次性创建响应。
    #[must_use]
    pub fn new(config_revision: u64, id: String, prefix: String, plaintext_key: String) -> Self {
        Self {
            config_revision,
            id,
            prefix,
            plaintext_key,
        }
    }
}

impl fmt::Debug for CreatedClientKeyData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatedClientKeyData")
            .field("config_revision", &self.config_revision)
            .field("id", &self.id)
            .field("prefix", &self.prefix)
            .field("plaintext_key", &"[REDACTED]")
            .finish()
    }
}

/// 仅由显式 reveal 返回一次的完整明文 Key。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevealedClientKeyData {
    id: String,
    plaintext_key: String,
}

impl RevealedClientKeyData {
    #[must_use]
    pub fn new(id: String, plaintext_key: String) -> Self {
        Self { id, plaintext_key }
    }
}

impl fmt::Debug for RevealedClientKeyData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevealedClientKeyData")
            .field("id", &self.id)
            .field("plaintext_key", &"[REDACTED]")
            .finish()
    }
}

/// Client Key mutation 响应数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutatedClientKeyData {
    config_revision: u64,
    id: String,
}

impl MutatedClientKeyData {
    /// 构造 mutation 响应。
    #[must_use]
    pub fn new(config_revision: u64, id: String) -> Self {
        Self {
            config_revision,
            id,
        }
    }
}

/// 解码后的 Client Key 游标字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyCursorData {
    pub sort: ClientKeySort,
    pub value: ClientKeyCursorValue,
    pub id: String,
}

/// 游标中与排序字段严格对应的最后一行值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ClientKeyCursorValue {
    Name(String),
    Enabled(bool),
    CreatedAt(DateTime<Utc>),
    LastUsedAt(Option<DateTime<Utc>>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CursorWire {
    sort: ClientKeySort,
    value: ClientKeyCursorValue,
    id: String,
}

/// 把 owner 游标编码为不透明 wire 值。
pub fn encode_client_key_cursor(
    cursor: &ClientKeyCursorData,
) -> Result<String, WireValidationError> {
    validate_client_key_cursor(cursor)?;
    let bytes = serde_json::to_vec(&CursorWire {
        sort: cursor.sort,
        value: cursor.value.clone(),
        id: cursor.id.clone(),
    })
    .map_err(|_| WireValidationError::new("cursor"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// 解码并严格校验 Client Key 游标。
pub fn decode_client_key_cursor(encoded: &str) -> Result<ClientKeyCursorData, WireValidationError> {
    if encoded.is_empty() || encoded.len() > MAX_CURSOR_BYTES {
        return Err(WireValidationError::new("cursor"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| WireValidationError::new("cursor"))?;
    let cursor: CursorWire =
        serde_json::from_slice(&bytes).map_err(|_| WireValidationError::new("cursor"))?;
    let cursor = ClientKeyCursorData {
        sort: cursor.sort,
        value: cursor.value,
        id: cursor.id,
    };
    validate_client_key_cursor(&cursor)?;
    Ok(cursor)
}

fn validate_client_key_cursor(cursor: &ClientKeyCursorData) -> Result<(), WireValidationError> {
    validate_required_text(&cursor.id, "cursor")?;
    let matching = matches!(
        (cursor.sort.field, &cursor.value),
        (ClientKeySortField::Name, ClientKeyCursorValue::Name(value)) if !value.trim().is_empty()
    ) || matches!(
        (cursor.sort.field, &cursor.value),
        (
            ClientKeySortField::Enabled,
            ClientKeyCursorValue::Enabled(_)
        ) | (
            ClientKeySortField::CreatedAt,
            ClientKeyCursorValue::CreatedAt(_)
        ) | (
            ClientKeySortField::LastUsedAt,
            ClientKeyCursorValue::LastUsedAt(_)
        )
    );
    if matching {
        Ok(())
    } else {
        Err(WireValidationError::new("cursor"))
    }
}

fn validate_revision(value: u64) -> Result<(), WireValidationError> {
    if value == 0 {
        return Err(WireValidationError::new("expectedConfigRevision"));
    }
    Ok(())
}

fn validate_limit(value: u64, field: &'static str) -> Result<(), WireValidationError> {
    if i64::try_from(value).is_err() {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn validate_required_text(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    if value.trim().is_empty() {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn validate_provider_kind(value: &str) -> Result<(), WireValidationError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (byte == b'-' && index > 0 && index + 1 < value.len())
        })
        && !value.starts_with('-')
        && !value.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(WireValidationError::new("providerKind"))
    }
}

fn validate_optional_text(
    value: Option<&str>,
    field: &'static str,
) -> Result<(), WireValidationError> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn contains_client_key_material(value: &str) -> bool {
    value.as_bytes().windows(46).any(|window| {
        &window[..3] == b"sk_"
            && window[3..]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

/// Client API Key 管理应用端口。
#[async_trait]
pub trait ClientKeyAdminService: Send + Sync {
    async fn list(
        &self,
        query: ListClientKeysFields,
    ) -> Result<ClientKeyListData, AdminServiceError>;

    async fn create(
        &self,
        context: &AdminRequestContext,
        fields: CreateClientKeyFields,
    ) -> Result<CreatedClientKeyData, AdminServiceError>;

    async fn reveal(&self, id: String) -> Result<RevealedClientKeyData, AdminServiceError>;

    async fn update(
        &self,
        context: &AdminRequestContext,
        fields: UpdateClientKeyFields,
    ) -> Result<MutatedClientKeyData, AdminServiceError>;

    async fn disable(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError>;

    async fn enable(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError>;

    async fn delete(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError>;
}

/// Client API Key HTTP module 所需最小 state。
pub trait ClientKeyAdminState: AdminSessionState {
    fn client_key_admin_service(&self) -> &dyn ClientKeyAdminService;
}

/// 构造固定 GET/POST 且 ID 仅位于 query/body 的 Client API Key 路由。
pub fn router<S>() -> Router<S>
where
    S: ClientKeyAdminState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/api/admin/client-keys",
            get(list_client_keys::<S>).post(create_client_key::<S>),
        )
        .route("/api/admin/client-keys/reveal", get(reveal_client_key::<S>))
        .route(
            "/api/admin/client-keys/update",
            post(update_client_key::<S>),
        )
        .route(
            "/api/admin/client-keys/disable",
            post(disable_client_key::<S>),
        )
        .route(
            "/api/admin/client-keys/enable",
            post(enable_client_key::<S>),
        )
        .route(
            "/api/admin/client-keys/delete",
            post(delete_client_key::<S>),
        )
}

async fn list_client_keys<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<ListClientKeysQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let fields = query.into_parts().map_err(map_wire_error)?;
    let data = state
        .client_key_admin_service()
        .list(fields)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn create_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<CreateClientKeyRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let fields = payload.into_fields().map_err(map_wire_error)?;
    let data = state
        .client_key_admin_service()
        .create(auth.context(), fields)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(data),
    ))
}

async fn reveal_client_key<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<ClientKeyIdQuery>,
) -> Result<Response, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let id = query.into_id().map_err(map_wire_error)?;
    let data = state
        .client_key_admin_service()
        .reveal(id)
        .await
        .map_err(map_service_error)?;
    let mut response = AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn update_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<UpdateClientKeyRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let fields = payload.into_fields().map_err(map_wire_error)?;
    mutation_response(
        state
            .client_key_admin_service()
            .update(auth.context(), fields)
            .await,
    )
}

async fn disable_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<ClientKeyRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let (id, revision) = payload.into_parts().map_err(map_wire_error)?;
    mutation_response(
        state
            .client_key_admin_service()
            .disable(auth.context(), id, revision)
            .await,
    )
}

async fn enable_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<ClientKeyRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let (id, revision) = payload.into_parts().map_err(map_wire_error)?;
    mutation_response(
        state
            .client_key_admin_service()
            .enable(auth.context(), id, revision)
            .await,
    )
}

async fn delete_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<ClientKeyRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: ClientKeyAdminState + Send + Sync,
{
    let (id, revision) = payload.into_parts().map_err(map_wire_error)?;
    mutation_response(
        state
            .client_key_admin_service()
            .delete(auth.context(), id, revision)
            .await,
    )
}

fn mutation_response(
    result: Result<MutatedClientKeyData, AdminServiceError>,
) -> Result<AdminResponse<AdminEnvelope<MutatedClientKeyData>>, AdminError> {
    let data = result.map_err(map_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    match error.field() {
        "cursor" => AdminError::bad_request("Invalid client key cursor"),
        "expectedConfigRevision" => {
            AdminError::bad_request("expectedConfigRevision must be positive")
        }
        _ => AdminError::bad_request("Invalid client API key request"),
    }
}

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminServiceErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminServiceErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminServiceErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminServiceErrorKind::Unavailable => {
            AdminError::service_unavailable("Configuration repository unavailable")
        }
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
