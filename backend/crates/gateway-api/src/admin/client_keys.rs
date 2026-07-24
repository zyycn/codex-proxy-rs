//! Client API Key 管理 wire contract。

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use gateway_admin::model::{
    Revision,
    client_keys::{
        ClientKeyCursor, ClientKeyCursorValue as DomainCursorValue, ClientKeyListQuery,
        ClientKeyMutation, ClientKeyPage, ClientKeyPageSize, ClientKeyRecord, ClientKeySecret,
        ClientKeySort as DomainSort, ClientKeySortField as DomainSortField, CreateClientKey,
        CreatedClientKey, DeleteClientKey, SetClientKeyEnabled, SortDirection, UpdateClientKey,
    },
};
use gateway_core::{
    policy::{ClientApiKeyId, RateLimits},
    routing::ProviderKind,
};
use serde::{Deserialize, Serialize};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, WireValidationError,
    wire::map_admin_service_error,
};

const MAX_CURSOR_BYTES: usize = 512;
const MAX_SEARCH_BYTES: usize = 256;
const DEFAULT_PAGE_SIZE: u16 = 50;

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
    /// 校验 wire 边界并直接构造管理用例查询。
    pub fn into_command(self) -> Result<ClientKeyListQuery, WireValidationError> {
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
        let cursor = self
            .cursor
            .as_deref()
            .map(decode_client_key_cursor)
            .transpose()?
            .map(domain_cursor)
            .transpose()?;
        let page_size = ClientKeyPageSize::new(self.limit.unwrap_or(DEFAULT_PAGE_SIZE))
            .map_err(|_| WireValidationError::new("limit"))?;
        Ok(ClientKeyListQuery {
            cursor,
            page_size,
            search: search.filter(|search| !search.is_empty()),
            sort: domain_sort(sort),
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
}

impl CreateClientKeyRequest {
    /// 校验 wire 边界并直接构造管理用例命令。
    pub fn into_command(self) -> Result<CreateClientKey, WireValidationError> {
        validate_revision(self.expected_config_revision)?;
        validate_required_text(&self.name, "name")?;
        validate_optional_text(self.label.as_deref(), "label")?;
        validate_provider_kind(&self.provider_kind)?;
        validate_limit(self.max_concurrency, "maxConcurrency")?;
        validate_limit(self.requests_per_minute, "requestsPerMinute")?;
        Ok(CreateClientKey {
            expected_config_revision: revision(self.expected_config_revision)?,
            name: self.name,
            label: self.label,
            provider_kind: ProviderKind::new(self.provider_kind)
                .map_err(|_| WireValidationError::new("providerKind"))?,
            limits: RateLimits {
                max_concurrency: self.max_concurrency,
                requests_per_minute: self.requests_per_minute,
            },
        })
    }
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
}

impl UpdateClientKeyRequest {
    /// 校验 wire 边界并直接构造管理用例命令。
    pub fn into_command(self) -> Result<UpdateClientKey, WireValidationError> {
        validate_required_text(&self.id, "id")?;
        validate_revision(self.expected_config_revision)?;
        validate_required_text(&self.name, "name")?;
        validate_optional_text(self.label.as_deref(), "label")?;
        validate_provider_kind(&self.provider_kind)?;
        validate_limit(self.max_concurrency, "maxConcurrency")?;
        validate_limit(self.requests_per_minute, "requestsPerMinute")?;
        Ok(UpdateClientKey {
            expected_config_revision: revision(self.expected_config_revision)?,
            id: client_key_id(self.id, "clientKeyMutationNotFound")?,
            name: self.name,
            label: self.label,
            provider_kind: ProviderKind::new(self.provider_kind)
                .map_err(|_| WireValidationError::new("providerKind"))?,
            limits: RateLimits {
                max_concurrency: self.max_concurrency,
                requests_per_minute: self.requests_per_minute,
            },
        })
    }
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

    fn into_domain_id(self) -> Result<ClientApiKeyId, WireValidationError> {
        client_key_id(self.into_id()?, "clientKeyRevealNotFound")
    }
}

impl ClientKeyRevisionRequest {
    /// 校验请求并取出 ID 与 revision。
    pub fn into_parts(self) -> Result<(String, u64), WireValidationError> {
        validate_required_text(&self.id, "id")?;
        validate_revision(self.expected_config_revision)?;
        Ok((self.id, self.expected_config_revision))
    }

    fn into_command_parts(self) -> Result<(ClientApiKeyId, Revision), WireValidationError> {
        let (id, expected_config_revision) = self.into_parts()?;
        Ok((
            client_key_id(id, "clientKeyMutationNotFound")?,
            revision(expected_config_revision)?,
        ))
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
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

impl From<ClientKeyRecord> for ClientKeyView {
    fn from(record: ClientKeyRecord) -> Self {
        Self {
            id: record.id.to_string(),
            name: record.name,
            label: record.label,
            provider_kind: record.provider_kind.to_string(),
            prefix: record.prefix,
            enabled: record.enabled,
            max_concurrency: record.limits.max_concurrency,
            requests_per_minute: record.limits.requests_per_minute,
            created_at: record.created_at,
            updated_at: record.updated_at,
            last_used_at: record.last_used_at,
        }
    }
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

impl TryFrom<ClientKeyPage> for ClientKeyListData {
    type Error = WireValidationError;

    fn try_from(page: ClientKeyPage) -> Result<Self, Self::Error> {
        let next_cursor = page
            .next_cursor
            .map(wire_cursor)
            .transpose()?
            .as_ref()
            .map(encode_client_key_cursor)
            .transpose()?;
        Ok(Self::new(
            page.config_revision.get(),
            page.items.into_iter().map(Into::into).collect(),
            next_cursor,
            page.total,
        ))
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

impl From<CreatedClientKey> for CreatedClientKeyData {
    fn from(created: CreatedClientKey) -> Self {
        Self::new(
            created.config_revision.get(),
            created.secret.record.id.to_string(),
            created.secret.record.prefix.clone(),
            created.secret.expose_for_response().to_owned(),
        )
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

impl From<ClientKeySecret> for RevealedClientKeyData {
    fn from(secret: ClientKeySecret) -> Self {
        Self::new(
            secret.record.id.to_string(),
            secret.expose_for_response().to_owned(),
        )
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

impl From<ClientKeyMutation> for MutatedClientKeyData {
    fn from(mutation: ClientKeyMutation) -> Self {
        Self::new(mutation.config_revision.get(), mutation.id.to_string())
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

const fn domain_sort(sort: ClientKeySort) -> DomainSort {
    DomainSort {
        field: match sort.field {
            ClientKeySortField::Name => DomainSortField::Name,
            ClientKeySortField::Enabled => DomainSortField::Enabled,
            ClientKeySortField::CreatedAt => DomainSortField::CreatedAt,
            ClientKeySortField::LastUsedAt => DomainSortField::LastUsedAt,
        },
        direction: match sort.direction {
            ClientKeySortDirection::Asc => SortDirection::Asc,
            ClientKeySortDirection::Desc => SortDirection::Desc,
        },
    }
}

const fn wire_sort(sort: DomainSort) -> ClientKeySort {
    ClientKeySort {
        field: match sort.field {
            DomainSortField::Name => ClientKeySortField::Name,
            DomainSortField::Enabled => ClientKeySortField::Enabled,
            DomainSortField::CreatedAt => ClientKeySortField::CreatedAt,
            DomainSortField::LastUsedAt => ClientKeySortField::LastUsedAt,
        },
        direction: match sort.direction {
            SortDirection::Asc => ClientKeySortDirection::Asc,
            SortDirection::Desc => ClientKeySortDirection::Desc,
        },
    }
}

fn domain_cursor(cursor: ClientKeyCursorData) -> Result<ClientKeyCursor, WireValidationError> {
    let value = match cursor.value {
        ClientKeyCursorValue::Name(value) => DomainCursorValue::Name(value),
        ClientKeyCursorValue::Enabled(value) => DomainCursorValue::Enabled(value),
        ClientKeyCursorValue::CreatedAt(value) => DomainCursorValue::CreatedAt(value),
        ClientKeyCursorValue::LastUsedAt(value) => DomainCursorValue::LastUsedAt(value),
    };
    Ok(ClientKeyCursor {
        sort: domain_sort(cursor.sort),
        value,
        id: client_key_id(cursor.id, "cursor")?,
    })
}

fn wire_cursor(cursor: ClientKeyCursor) -> Result<ClientKeyCursorData, WireValidationError> {
    let value = match cursor.value {
        DomainCursorValue::Name(value) => ClientKeyCursorValue::Name(value),
        DomainCursorValue::Enabled(value) => ClientKeyCursorValue::Enabled(value),
        DomainCursorValue::CreatedAt(value) => ClientKeyCursorValue::CreatedAt(value),
        DomainCursorValue::LastUsedAt(value) => ClientKeyCursorValue::LastUsedAt(value),
    };
    let cursor = ClientKeyCursorData {
        sort: wire_sort(cursor.sort),
        value,
        id: cursor.id.to_string(),
    };
    validate_client_key_cursor(&cursor)?;
    Ok(cursor)
}

fn validate_revision(value: u64) -> Result<(), WireValidationError> {
    if value == 0 {
        return Err(WireValidationError::new("expectedConfigRevision"));
    }
    Ok(())
}

fn revision(value: u64) -> Result<Revision, WireValidationError> {
    validate_revision(value)?;
    Revision::new(value).map_err(|_| WireValidationError::new("expectedConfigRevision"))
}

fn client_key_id(
    value: String,
    field: &'static str,
) -> Result<ClientApiKeyId, WireValidationError> {
    ClientApiKeyId::new(value).map_err(|_| WireValidationError::new(field))
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

/// 构造固定 GET/POST 且 ID 仅位于 query/body 的 Client API Key 路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/client-keys", get(list_client_keys::<S>))
        .route(
            "/api/admin/client-keys/create",
            post(create_client_key::<S>),
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
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .client_keys()
        .list(query.into_command().map_err(map_wire_error)?)
        .await
        .map_err(map_service_error)?;
    let data = ClientKeyListData::try_from(result)
        .map_err(|_| AdminError::internal("Failed to encode client key cursor"))?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn create_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<CreateClientKeyRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = payload.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .client_keys()
        .create(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(CreatedClientKeyData::from(result)),
    ))
}

async fn reveal_client_key<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<ClientKeyIdQuery>,
) -> Result<Response, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let id = query.into_domain_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .client_keys()
        .reveal(&id)
        .await
        .map_err(map_service_error)?;
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RevealedClientKeyData::from(result)),
    )
    .into_response();
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
    S: AdminSessionState + Send + Sync,
{
    let command = payload.into_command().map_err(map_wire_error)?;
    mutation_response(
        state
            .admin_services()
            .client_keys()
            .update(&auth.context().mutation_context(), command)
            .await,
    )
}

async fn disable_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<ClientKeyRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (id, expected_config_revision) = payload.into_command_parts().map_err(map_wire_error)?;
    mutation_response(
        state
            .admin_services()
            .client_keys()
            .set_enabled(
                &auth.context().mutation_context(),
                SetClientKeyEnabled {
                    expected_config_revision,
                    id,
                    enabled: false,
                },
            )
            .await,
    )
}

async fn enable_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<ClientKeyRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (id, expected_config_revision) = payload.into_command_parts().map_err(map_wire_error)?;
    mutation_response(
        state
            .admin_services()
            .client_keys()
            .set_enabled(
                &auth.context().mutation_context(),
                SetClientKeyEnabled {
                    expected_config_revision,
                    id,
                    enabled: true,
                },
            )
            .await,
    )
}

async fn delete_client_key<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(payload): Json<ClientKeyRevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (id, expected_config_revision) = payload.into_command_parts().map_err(map_wire_error)?;
    mutation_response(
        state
            .admin_services()
            .client_keys()
            .delete(
                &auth.context().mutation_context(),
                DeleteClientKey {
                    expected_config_revision,
                    id,
                },
            )
            .await,
    )
}

fn mutation_response(
    result: Result<ClientKeyMutation, gateway_admin::model::AdminError>,
) -> Result<AdminResponse<AdminEnvelope<MutatedClientKeyData>>, AdminError> {
    let data = result.map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(MutatedClientKeyData::from(data)),
    ))
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    match error.field() {
        "cursor" => AdminError::bad_request("Invalid client key cursor"),
        "clientKeyRevealNotFound" => AdminError::not_found("Client API key was not found"),
        "clientKeyMutationNotFound" => AdminError::not_found("client API key was not found"),
        "expectedConfigRevision" => {
            AdminError::bad_request("expectedConfigRevision must be positive")
        }
        _ => AdminError::bad_request("Invalid client API key request"),
    }
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error, "Configuration repository unavailable")
}
