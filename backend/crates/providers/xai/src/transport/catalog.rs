//! 官方 Grok CLI proxy 模型目录 wire、transport port 与安全快照。

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::Arc;

use gateway_core::routing::UpstreamModelId;
use serde::Deserialize;
use serde_json::{Map, Value};
use url::Url;
use zeroize::Zeroizing;

use crate::{GrokHeader, SecretValue};

/// 官方 Grok CLI proxy 模型目录 URL。
pub const GROK_MODEL_CATALOG_URL: &str = "https://cli-chat-proxy.grok.com/v1/models";
/// 官方 Grok Build credits/billing URL。
pub const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
/// 单次 Grok 模型目录响应允许的最大字节数。
pub const MAX_GROK_MODEL_CATALOG_BYTES: usize = 1024 * 1024;
/// 单次 Grok billing 响应允许的最大字节数。
pub const MAX_GROK_BILLING_BYTES: usize = 512 * 1024;

pub(crate) const MAX_CATALOG_MODELS: usize = 2_048;
const MAX_DISPLAY_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_ETAG_BYTES: usize = 256;
const CLIENT_MODE: &str = "headless";

/// 构造模型目录 OAuth session 失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokModelCatalogSessionError {
    /// OAuth、身份或兼容版本无法安全写入 HTTP header。
    #[error("Grok model catalog OAuth session contains invalid header data")]
    InvalidHeaderData,
}

/// 一次模型目录请求使用的 OAuth session，不支持 API Key。
#[derive(Clone)]
pub struct GrokModelCatalogSession {
    access_token: SecretValue,
    user_id: SecretValue,
    email: Option<SecretValue>,
    client_version: String,
}

impl GrokModelCatalogSession {
    /// 创建仅用于官方 CLI proxy 的 OAuth session。
    ///
    /// # Errors
    ///
    /// 任一 header 值为空、过长或含非可见 ASCII 时返回错误。
    pub fn new(
        access_token: SecretValue,
        user_id: SecretValue,
        email: Option<SecretValue>,
        client_version: impl Into<String>,
    ) -> Result<Self, GrokModelCatalogSessionError> {
        let client_version = client_version.into();
        if !valid_secret_header(&access_token, 64 * 1024)
            || !valid_secret_header(&user_id, 1_024)
            || email
                .as_ref()
                .is_some_and(|value| !valid_secret_header(value, 1_024))
            || !valid_header_atom(&client_version, 64)
        {
            return Err(GrokModelCatalogSessionError::InvalidHeaderData);
        }
        Ok(Self {
            access_token,
            user_id,
            email,
            client_version,
        })
    }
}

impl fmt::Debug for GrokModelCatalogSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokModelCatalogSession")
            .field("access_token", &"[REDACTED]")
            .field("user_id", &"[REDACTED]")
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field("client_version", &self.client_version)
            .finish()
    }
}

/// 交给单次 GET transport 的完整模型目录请求。
#[derive(Debug)]
pub struct GrokModelCatalogRequest {
    endpoint: Url,
    headers: Vec<GrokHeader>,
}

impl GrokModelCatalogRequest {
    fn from_session(session: &GrokModelCatalogSession) -> Result<Self, GrokModelCatalogError> {
        let endpoint = Url::parse(GROK_MODEL_CATALOG_URL)
            .map_err(|_| GrokModelCatalogError::InvalidRequest)?;
        let mut headers = vec![
            GrokHeader::sensitive(
                "authorization",
                SecretValue::new(format!("Bearer {}", session.access_token.expose())),
            ),
            GrokHeader::public("X-XAI-Token-Auth", "xai-grok-cli"),
            GrokHeader::sensitive("x-userid", session.user_id.clone()),
            GrokHeader::public("x-grok-client-version", session.client_version.clone()),
            GrokHeader::public("x-grok-client-mode", CLIENT_MODE),
            GrokHeader::public("accept", "application/json"),
        ];
        if let Some(email) = &session.email {
            headers.push(GrokHeader::sensitive("x-email", email.clone()));
        }
        Ok(Self { endpoint, headers })
    }

    /// 返回固定官方 `/v1/models` URL。
    #[must_use]
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// 返回完整官方 OAuth/session header；敏感值保留类型标记。
    #[must_use]
    pub fn headers(&self) -> &[GrokHeader] {
        &self.headers
    }
}

/// 单次成功 GET 的有界原始响应。
pub struct GrokModelCatalogTransportResponse {
    body: Zeroizing<Vec<u8>>,
    etag: Option<String>,
}

impl GrokModelCatalogTransportResponse {
    /// 创建 transport 响应；上层仍会重复执行字节和 ETag 校验。
    #[must_use]
    pub fn new(body: impl Into<Vec<u8>>, etag: Option<String>) -> Self {
        Self {
            body: Zeroizing::new(body.into()),
            etag,
        }
    }

    fn body(&self) -> &[u8] {
        &self.body
    }

    fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }
}

impl fmt::Debug for GrokModelCatalogTransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokModelCatalogTransportResponse")
            .field("body", &"[REDACTED]")
            .field("body_len", &self.body.len())
            .field("etag", &self.etag.as_ref().map(|_| "[UNVALIDATED]"))
            .finish()
    }
}

/// 模型目录 GET transport 失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokModelCatalogTransportErrorKind {
    /// URL、header 或响应协议不合法。
    Protocol,
    /// OAuth access token 被拒绝。
    Unauthorized,
    /// OAuth session 没有目录权限。
    PermissionDenied,
    /// 官方 proxy 限流。
    RateLimited,
    /// 请求超时。
    Timeout,
    /// 网络或 TLS 失败。
    Transport,
    /// 官方 proxy 暂不可用。
    Unavailable,
}

/// 不携带上游正文的模型目录 transport 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Grok model catalog transport failed: {kind:?}")]
pub struct GrokModelCatalogTransportError {
    kind: GrokModelCatalogTransportErrorKind,
    status: Option<u16>,
}

impl GrokModelCatalogTransportError {
    /// 创建未附带 HTTP 状态的失败。
    #[must_use]
    pub const fn new(kind: GrokModelCatalogTransportErrorKind) -> Self {
        Self { kind, status: None }
    }

    /// 附加合法 HTTP 状态码。
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        if (100..=599).contains(&status) {
            self.status = Some(status);
        }
        self
    }

    /// 返回稳定失败分类。
    #[must_use]
    pub const fn kind(&self) -> GrokModelCatalogTransportErrorKind {
        self.kind
    }

    /// 返回可公开 HTTP 状态。
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }
}

/// 模型目录 transport future。
pub type GrokModelCatalogTransportFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<GrokModelCatalogTransportResponse, GrokModelCatalogTransportError>,
            > + Send
            + 'a,
    >,
>;

/// 只执行一次固定官方 GET 的模型目录 transport port。
pub trait GrokModelCatalogTransport: Send + Sync {
    /// 发送一次请求；实现不得重试、改用 API Key 或跟随 redirect。
    fn execute(&self, request: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_>;
}

/// 交给单次官方 billing GET transport 的完整请求。
#[derive(Debug)]
pub struct GrokBillingRequest {
    endpoint: Url,
    headers: Vec<GrokHeader>,
}

impl GrokBillingRequest {
    fn from_session(session: &GrokModelCatalogSession) -> Result<Self, GrokBillingError> {
        let endpoint =
            Url::parse(GROK_BILLING_URL).map_err(|_| GrokBillingError::InvalidRequest)?;
        let mut headers = vec![
            GrokHeader::sensitive(
                "authorization",
                SecretValue::new(format!("Bearer {}", session.access_token.expose())),
            ),
            GrokHeader::public("X-XAI-Token-Auth", "xai-grok-cli"),
            GrokHeader::sensitive("x-userid", session.user_id.clone()),
            GrokHeader::public("x-grok-client-version", session.client_version.clone()),
            GrokHeader::public("x-grok-client-mode", CLIENT_MODE),
            GrokHeader::public("accept", "application/json"),
        ];
        if let Some(email) = &session.email {
            headers.push(GrokHeader::sensitive("x-email", email.clone()));
        }
        Ok(Self { endpoint, headers })
    }

    /// 返回固定官方 `/v1/billing?format=credits` URL。
    #[must_use]
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// 返回官方 OAuth/session headers；敏感值保留类型标记。
    #[must_use]
    pub fn headers(&self) -> &[GrokHeader] {
        &self.headers
    }
}

/// 单次成功 billing GET 的有界原始响应。
pub struct GrokBillingTransportResponse {
    body: Zeroizing<Vec<u8>>,
}

impl GrokBillingTransportResponse {
    #[must_use]
    pub fn new(body: impl Into<Vec<u8>>) -> Self {
        Self {
            body: Zeroizing::new(body.into()),
        }
    }

    fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for GrokBillingTransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokBillingTransportResponse")
            .field("body", &"[REDACTED]")
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Billing transport 的稳定失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokBillingTransportErrorKind {
    Protocol,
    Unauthorized,
    PermissionDenied,
    RateLimited,
    Timeout,
    Transport,
    Unavailable,
}

/// 不携带上游正文的 billing transport 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Grok billing transport failed: {kind:?}")]
pub struct GrokBillingTransportError {
    kind: GrokBillingTransportErrorKind,
    status: Option<u16>,
}

impl GrokBillingTransportError {
    #[must_use]
    pub const fn new(kind: GrokBillingTransportErrorKind) -> Self {
        Self { kind, status: None }
    }

    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        if (100..=599).contains(&status) {
            self.status = Some(status);
        }
        self
    }

    #[must_use]
    pub const fn kind(&self) -> GrokBillingTransportErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }
}

/// Billing transport future。
pub type GrokBillingTransportFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<GrokBillingTransportResponse, GrokBillingTransportError>>
            + Send
            + 'a,
    >,
>;

/// 只执行一次固定官方 billing GET 的 transport port。
pub trait GrokBillingTransport: Send + Sync {
    /// 实现不得重试、改用 API Key 或跟随 redirect。
    fn execute(&self, request: GrokBillingRequest) -> GrokBillingTransportFuture<'_>;
}

/// 已验证但仍由 xAI Provider 独占解释的动态 billing JSON。
#[derive(Clone, PartialEq)]
pub struct GrokBillingSnapshot {
    document: Map<String, Value>,
}

impl GrokBillingSnapshot {
    #[must_use]
    pub const fn document(&self) -> &Map<String, Value> {
        &self.document
    }

    #[must_use]
    pub fn into_document(self) -> Map<String, Value> {
        self.document
    }
}

impl fmt::Debug for GrokBillingSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokBillingSnapshot")
            .field("keys", &self.document.keys().collect::<Vec<_>>())
            .field("values", &"<provider-owned>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrokBillingError {
    #[error("Grok billing request is invalid")]
    InvalidRequest,
    #[error("Grok billing transport failed")]
    Transport,
    #[error("Grok billing response exceeds the byte limit")]
    ResponseTooLarge,
    #[error("Grok billing response is invalid")]
    InvalidWire,
}

/// 有界官方 billing client。
#[derive(Clone)]
pub struct GrokBillingClient {
    transport: Arc<dyn GrokBillingTransport>,
}

impl GrokBillingClient {
    #[must_use]
    pub fn new(transport: Arc<dyn GrokBillingTransport>) -> Self {
        Self { transport }
    }

    pub async fn fetch(
        &self,
        session: &GrokModelCatalogSession,
    ) -> Result<GrokBillingSnapshot, GrokBillingError> {
        let request = GrokBillingRequest::from_session(session)?;
        let response = self
            .transport
            .execute(request)
            .await
            .map_err(|_| GrokBillingError::Transport)?;
        parse_grok_billing(response.body())
    }
}

impl fmt::Debug for GrokBillingClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokBillingClient")
            .field("transport", &"[BILLING_TRANSPORT]")
            .finish()
    }
}

/// 验证已限长的动态 billing document，同时保留 Provider 后续新增字段。
pub fn parse_grok_billing(body: &[u8]) -> Result<GrokBillingSnapshot, GrokBillingError> {
    if body.len() > MAX_GROK_BILLING_BYTES {
        return Err(GrokBillingError::ResponseTooLarge);
    }
    let Value::Object(document) =
        serde_json::from_slice::<Value>(body).map_err(|_| GrokBillingError::InvalidWire)?
    else {
        return Err(GrokBillingError::InvalidWire);
    };
    validate_optional_billing_fields(&document)?;
    Ok(GrokBillingSnapshot { document })
}

fn validate_optional_billing_fields(document: &Map<String, Value>) -> Result<(), GrokBillingError> {
    if let Some(value) = document.get("config") {
        match value {
            Value::Null => {}
            Value::Object(config) => validate_billing_config(config)?,
            _ => return Err(GrokBillingError::InvalidWire),
        }
    }
    if document
        .get("onDemandEnabled")
        .is_some_and(|value| !value.is_null() && !value.is_boolean())
    {
        return Err(GrokBillingError::InvalidWire);
    }
    if let Some(value) = document.get("subscriptionTier")
        && !value.is_null()
    {
        validate_dynamic_text(value, 512)?;
    }
    Ok(())
}

fn validate_billing_config(config: &Map<String, Value>) -> Result<(), GrokBillingError> {
    if let Some(percent) = config.get("creditUsagePercent")
        && !percent.is_null()
    {
        let percent = percent.as_f64().filter(|value| value.is_finite());
        if !matches!(percent, Some(value) if (0.0..=100.0).contains(&value)) {
            return Err(GrokBillingError::InvalidWire);
        }
    }
    for field in [
        "monthlyLimit",
        "used",
        "onDemandCap",
        "onDemandUsed",
        "prepaidBalance",
    ] {
        if let Some(value) = config.get(field)
            && !value.is_null()
        {
            validate_cent(value)?;
        }
    }
    if let Some(period) = config.get("currentPeriod")
        && !period.is_null()
    {
        let Value::Object(period) = period else {
            return Err(GrokBillingError::InvalidWire);
        };
        for field in ["type", "start", "end"] {
            if let Some(value) = period.get(field)
                && !value.is_null()
            {
                validate_dynamic_text(value, 256)?;
            }
        }
    }
    if let Some(history) = config.get("history")
        && !history.is_null()
    {
        let Value::Array(history) = history else {
            return Err(GrokBillingError::InvalidWire);
        };
        if history.len() > 120 || history.iter().any(|value| !value.is_object()) {
            return Err(GrokBillingError::InvalidWire);
        }
    }
    Ok(())
}

fn validate_cent(value: &Value) -> Result<(), GrokBillingError> {
    let Value::Object(cent) = value else {
        return Err(GrokBillingError::InvalidWire);
    };
    if let Some(value) = cent.get("val")
        && value.as_i64().is_none_or(|value| value < 0)
    {
        return Err(GrokBillingError::InvalidWire);
    }
    Ok(())
}

fn validate_dynamic_text(value: &Value, max_bytes: usize) -> Result<(), GrokBillingError> {
    let Some(value) = value.as_str() else {
        return Err(GrokBillingError::InvalidWire);
    };
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(GrokBillingError::InvalidWire);
    }
    Ok(())
}

/// 上游目录对一项能力给出的明确证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokCatalogCapabilityEvidence {
    /// 上游明确声明原生支持。
    DeclaredNative,
    /// 上游明确声明不支持。
    DeclaredUnsupported,
    /// 上游没有提供可依赖的声明。
    Unknown,
}

impl GrokCatalogCapabilityEvidence {
    fn from_wire(value: Option<bool>) -> Self {
        match value {
            Some(true) => Self::DeclaredNative,
            Some(false) => Self::DeclaredUnsupported,
            None => Self::Unknown,
        }
    }
}

/// Grok 官方目录声明的 API backend。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokCatalogApiBackend {
    /// OpenAI Responses wire。
    Responses,
    /// OpenAI Chat Completions wire。
    ChatCompletions,
    /// Anthropic Messages wire。
    Messages,
}

/// Grok 目录中允许进入控制面的能力证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokCatalogCapabilities {
    responses_api: GrokCatalogCapabilityEvidence,
    reasoning_effort: GrokCatalogCapabilityEvidence,
    backend_search: GrokCatalogCapabilityEvidence,
    streaming_tool_calls: GrokCatalogCapabilityEvidence,
    api_backend: Option<GrokCatalogApiBackend>,
}

impl GrokCatalogCapabilities {
    /// 返回 Responses API 支持证据。
    #[must_use]
    pub const fn responses_api(&self) -> GrokCatalogCapabilityEvidence {
        self.responses_api
    }

    /// 返回 reasoning effort 支持证据。
    #[must_use]
    pub const fn reasoning_effort(&self) -> GrokCatalogCapabilityEvidence {
        self.reasoning_effort
    }

    /// 返回后端 search 支持证据。
    #[must_use]
    pub const fn backend_search(&self) -> GrokCatalogCapabilityEvidence {
        self.backend_search
    }

    /// 返回流式工具调用支持证据。
    #[must_use]
    pub const fn streaming_tool_calls(&self) -> GrokCatalogCapabilityEvidence {
        self.streaming_tool_calls
    }

    /// 返回上游明确声明的 API backend。
    #[must_use]
    pub const fn api_backend(&self) -> Option<GrokCatalogApiBackend> {
        self.api_backend
    }
}

/// Grok 目录中明确声明的模型限制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokCatalogLimits {
    context_window_tokens: Option<NonZeroU64>,
    max_output_tokens: Option<NonZeroU64>,
}

impl GrokCatalogLimits {
    /// 返回明确上下文窗口；缺失表示未知。
    #[must_use]
    pub const fn context_window_tokens(&self) -> Option<NonZeroU64> {
        self.context_window_tokens
    }

    /// 返回明确最大输出 token；缺失表示未知。
    #[must_use]
    pub const fn max_output_tokens(&self) -> Option<NonZeroU64> {
        self.max_output_tokens
    }
}

/// Grok 目录中允许持久化的原始元数据白名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokCatalogMetadata {
    catalog_entry_id: Option<String>,
    description: Option<String>,
    hidden: Option<bool>,
}

impl GrokCatalogMetadata {
    /// 返回目录自身的稳定 entry ID；它不替代实际请求模型。
    #[must_use]
    pub fn catalog_entry_id(&self) -> Option<&str> {
        self.catalog_entry_id.as_deref()
    }

    /// 返回安全说明文本。
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// 返回上游明确隐藏标记。
    #[must_use]
    pub const fn hidden(&self) -> Option<bool> {
        self.hidden
    }
}

/// 一个已完整校验的 Grok 真实模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokCatalogModel {
    request_model: UpstreamModelId,
    display_name: Option<String>,
    capabilities: GrokCatalogCapabilities,
    limits: GrokCatalogLimits,
    metadata: GrokCatalogMetadata,
}

impl GrokCatalogModel {
    /// 返回实际写入上游请求的模型 slug。
    #[must_use]
    pub const fn request_model(&self) -> &UpstreamModelId {
        &self.request_model
    }

    /// 返回上游明确提供的展示名；不从 slug 猜测。
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// 返回能力证据。
    #[must_use]
    pub const fn capabilities(&self) -> &GrokCatalogCapabilities {
        &self.capabilities
    }

    /// 返回明确限制。
    #[must_use]
    pub const fn limits(&self) -> &GrokCatalogLimits {
        &self.limits
    }

    /// 返回白名单元数据。
    #[must_use]
    pub const fn metadata(&self) -> &GrokCatalogMetadata {
        &self.metadata
    }
}

/// 一次完整成功的 Grok 远端模型快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokModelCatalogSnapshot {
    models: Vec<GrokCatalogModel>,
    etag: Option<String>,
}

impl GrokModelCatalogSnapshot {
    /// 返回本轮全部模型。
    #[must_use]
    pub fn models(&self) -> &[GrokCatalogModel] {
        &self.models
    }

    /// 返回经过语法和长度白名单校验的 HTTP ETag。
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }
}

/// 模型目录请求或完整快照失败。
#[derive(Debug, thiserror::Error)]
pub enum GrokModelCatalogError {
    /// 固定官方请求无法构造。
    #[error("Grok model catalog request is invalid")]
    InvalidRequest,
    /// 单次 transport 失败。
    #[error(transparent)]
    Transport(#[from] GrokModelCatalogTransportError),
    /// 响应超过硬上限。
    #[error("Grok model catalog response exceeds the byte limit")]
    ResponseTooLarge,
    /// 顶层不是唯一受支持的 `{data:[...]}` wire。
    #[error("Grok model catalog response violates the official wire contract")]
    InvalidWire,
    /// 目录没有任何模型。
    #[error("Grok model catalog snapshot is empty")]
    EmptySnapshot,
    /// 目录模型数量超过安全上限。
    #[error("Grok model catalog contains too many models")]
    TooManyModels,
    /// 实际请求模型 slug 不合法。
    #[error("Grok model catalog contains an invalid request model slug")]
    InvalidModelSlug,
    /// 同一实际请求模型重复出现。
    #[error("Grok model catalog contains a duplicate request model slug")]
    DuplicateModelSlug,
    /// 展示或白名单元数据不合法。
    #[error("Grok model catalog contains invalid public metadata")]
    InvalidMetadata,
    /// 模型限制不是明确的正整数。
    #[error("Grok model catalog contains invalid model limits")]
    InvalidLimits,
    /// ETag 不属于允许持久化的安全格式。
    #[error("Grok model catalog ETag is invalid")]
    InvalidEtag,
}

/// 仅负责编排一次严格 transport 与快照解析的目录 client。
pub struct GrokModelCatalogClient {
    transport: Arc<dyn GrokModelCatalogTransport>,
}

impl GrokModelCatalogClient {
    /// 注入一个严格的单次 GET transport。
    #[must_use]
    pub fn new(transport: Arc<dyn GrokModelCatalogTransport>) -> Self {
        Self { transport }
    }

    /// 获取并验证一次完整快照。
    ///
    /// # Errors
    ///
    /// transport、字节上限、wire、任一模型或 ETag 不合法时整轮失败。
    pub async fn fetch(
        &self,
        session: &GrokModelCatalogSession,
    ) -> Result<GrokModelCatalogSnapshot, GrokModelCatalogError> {
        let request = GrokModelCatalogRequest::from_session(session)?;
        let response = self.transport.execute(request).await?;
        parse_grok_model_catalog(response.body(), response.etag())
    }
}

impl fmt::Debug for GrokModelCatalogClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokModelCatalogClient")
            .field("transport", &"dyn GrokModelCatalogTransport")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrokModelsWire {
    object: GrokModelsObject,
    data: Vec<GrokModelWire>,
}

#[derive(Debug, Deserialize)]
enum GrokModelsObject {
    #[serde(rename = "list")]
    List,
}

#[derive(Debug, Deserialize)]
struct GrokModelWire {
    id: Option<String>,
    model: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "contextWindow", alias = "context_window")]
    context_window: Option<u64>,
    #[serde(rename = "maxCompletionTokens", alias = "max_completion_tokens")]
    max_completion_tokens: Option<u64>,
    #[serde(rename = "apiBackend", alias = "api_backend")]
    api_backend: Option<GrokCatalogApiBackend>,
    #[serde(rename = "supportedInApi", alias = "supported_in_api")]
    supported_in_api: Option<bool>,
    #[serde(
        rename = "supportsReasoningEffort",
        alias = "supports_reasoning_effort"
    )]
    supports_reasoning_effort: Option<bool>,
    #[serde(rename = "supportsBackendSearch", alias = "supports_backend_search")]
    supports_backend_search: Option<bool>,
    #[serde(rename = "streamToolCalls", alias = "stream_tool_calls")]
    stream_tool_calls: Option<bool>,
    hidden: Option<bool>,
}

/// 解析唯一受支持的 Grok 官方完整快照。
///
/// # Errors
///
/// 任一条目、分页信号、重复 slug、未知顶层字段或 ETag 不合法时整轮失败。
pub fn parse_grok_model_catalog(
    body: &[u8],
    etag: Option<&str>,
) -> Result<GrokModelCatalogSnapshot, GrokModelCatalogError> {
    if body.len() > MAX_GROK_MODEL_CATALOG_BYTES {
        return Err(GrokModelCatalogError::ResponseTooLarge);
    }
    let wire: GrokModelsWire =
        serde_json::from_slice(body).map_err(|_| GrokModelCatalogError::InvalidWire)?;
    let GrokModelsWire {
        object: GrokModelsObject::List,
        data,
    } = wire;
    if data.is_empty() {
        return Err(GrokModelCatalogError::EmptySnapshot);
    }
    if data.len() > MAX_CATALOG_MODELS {
        return Err(GrokModelCatalogError::TooManyModels);
    }
    let etag = etag.map(validate_etag).transpose()?;

    let mut seen = BTreeSet::new();
    let mut models = Vec::with_capacity(data.len());
    for model in data {
        let model = normalize_model(model)?;
        if !seen.insert(model.request_model.as_str().to_owned()) {
            return Err(GrokModelCatalogError::DuplicateModelSlug);
        }
        models.push(model);
    }
    Ok(GrokModelCatalogSnapshot { models, etag })
}

fn normalize_model(wire: GrokModelWire) -> Result<GrokCatalogModel, GrokModelCatalogError> {
    let actual_model = wire.model.or(wire.model_id).or_else(|| wire.id.clone());
    let actual_model = actual_model.ok_or(GrokModelCatalogError::InvalidModelSlug)?;
    if !valid_model_slug(&actual_model) {
        return Err(GrokModelCatalogError::InvalidModelSlug);
    }
    let request_model =
        UpstreamModelId::new(actual_model).map_err(|_| GrokModelCatalogError::InvalidModelSlug)?;

    if let Some(id) = wire.id.as_deref()
        && !valid_model_slug(id)
    {
        return Err(GrokModelCatalogError::InvalidMetadata);
    }
    if let Some(name) = wire.name.as_deref() {
        validate_public_text(name, MAX_DISPLAY_NAME_BYTES, false)?;
    }
    if let Some(description) = wire.description.as_deref() {
        validate_public_text(description, MAX_DESCRIPTION_BYTES, true)?;
    }

    let context_window_tokens = optional_positive(wire.context_window)?;
    let max_output_tokens = optional_positive(wire.max_completion_tokens)?;
    let responses_api = responses_evidence(wire.supported_in_api, wire.api_backend);

    Ok(GrokCatalogModel {
        request_model,
        display_name: wire.name,
        capabilities: GrokCatalogCapabilities {
            responses_api,
            reasoning_effort: GrokCatalogCapabilityEvidence::from_wire(
                wire.supports_reasoning_effort,
            ),
            backend_search: GrokCatalogCapabilityEvidence::from_wire(wire.supports_backend_search),
            streaming_tool_calls: GrokCatalogCapabilityEvidence::from_wire(wire.stream_tool_calls),
            api_backend: wire.api_backend,
        },
        limits: GrokCatalogLimits {
            context_window_tokens,
            max_output_tokens,
        },
        metadata: GrokCatalogMetadata {
            catalog_entry_id: wire.id,
            description: wire.description,
            hidden: wire.hidden,
        },
    })
}

fn responses_evidence(
    supported_in_api: Option<bool>,
    api_backend: Option<GrokCatalogApiBackend>,
) -> GrokCatalogCapabilityEvidence {
    match (supported_in_api, api_backend) {
        (Some(false), _)
        | (_, Some(GrokCatalogApiBackend::ChatCompletions | GrokCatalogApiBackend::Messages)) => {
            GrokCatalogCapabilityEvidence::DeclaredUnsupported
        }
        (None | Some(true), Some(GrokCatalogApiBackend::Responses)) => {
            GrokCatalogCapabilityEvidence::DeclaredNative
        }
        _ => GrokCatalogCapabilityEvidence::Unknown,
    }
}

fn optional_positive(value: Option<u64>) -> Result<Option<NonZeroU64>, GrokModelCatalogError> {
    value
        .map(|value| NonZeroU64::new(value).ok_or(GrokModelCatalogError::InvalidLimits))
        .transpose()
}

pub(crate) fn valid_model_slug(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_alphanumeric())
        && value.len() <= 256
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !value.starts_with("__")
}

fn validate_public_text(
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), GrokModelCatalogError> {
    if (!allow_empty && value.trim().is_empty())
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
    {
        return Err(GrokModelCatalogError::InvalidMetadata);
    }
    Ok(())
}

pub(crate) fn validate_etag(value: &str) -> Result<String, GrokModelCatalogError> {
    let tag = value.strip_prefix("W/").unwrap_or(value);
    let bytes = tag.as_bytes();
    if value.len() > MAX_ETAG_BYTES
        || bytes.len() < 2
        || bytes.first() != Some(&b'"')
        || bytes.last() != Some(&b'"')
        || !bytes[1..bytes.len() - 1]
            .iter()
            .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte))
    {
        return Err(GrokModelCatalogError::InvalidEtag);
    }
    Ok(value.to_owned())
}

fn valid_secret_header(value: &SecretValue, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .expose()
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte))
}

fn valid_header_atom(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}
