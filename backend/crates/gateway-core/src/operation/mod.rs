//! 协议无关的业务 operation。
//!
//! 这里只保留网关需要解释、路由或结算的稳定语义。协议 adapter 无法写入
//! wire body 与连接级事实随 [`ProtocolPayload`] 或 [`RawJsonPayload`] 不透明传递，
//! 由对应 Provider 自己解释。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::validation::{OperationError, validate_text};

/// 网关支持的稳定 operation 分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum OperationKind {
    /// 文本、多模态和工具生成。
    Generate,
    /// 图像生成。
    GenerateImage,
    /// Provider 原生 standalone search。
    Search,
    /// 使用目标模型的真实 tokenizer 计数，不允许 Core 估算。
    CountTokens,
    /// Provider 声明并映射到固定上游目标的账号认证 HTTP 操作。
    ProviderHttp,
}

impl OperationKind {
    /// 返回注册表和持久化使用的稳定名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::GenerateImage => "generate_image",
            Self::Search => "search",
            Self::CountTokens => "count_tokens",
            Self::ProviderHttp => "provider_http",
        }
    }
}

/// Router 理解的稳定能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Tool calling。
    Tools,
    /// 图像输入。
    Vision,
    /// 推理控制或推理输出。
    Reasoning,
    /// JSON Schema 输出。
    JsonSchema,
    /// Provider 原生延续。
    NativeContinuation,
}

/// 从 operation 推导出的请求能力约束。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityRequirements {
    operation: OperationKind,
    features: BTreeSet<Feature>,
    requested_output_tokens: Option<u64>,
}

impl CapabilityRequirements {
    /// 创建仅要求 operation 的能力约束。
    #[must_use]
    pub fn new(operation: OperationKind) -> Self {
        Self {
            operation,
            features: BTreeSet::new(),
            requested_output_tokens: None,
        }
    }

    /// 增加稳定能力要求。
    #[must_use]
    pub fn require(mut self, feature: Feature) -> Self {
        self.features.insert(feature);
        self
    }

    /// 设置请求的最大输出 token 数。
    #[must_use]
    pub const fn with_requested_output_tokens(mut self, tokens: Option<u64>) -> Self {
        self.requested_output_tokens = tokens;
        self
    }

    /// 返回 operation 分类。
    #[must_use]
    pub const fn operation(&self) -> OperationKind {
        self.operation
    }

    /// 返回全部稳定能力要求。
    #[must_use]
    pub fn features(&self) -> &BTreeSet<Feature> {
        &self.features
    }

    /// 返回请求的最大输出 token 数。
    #[must_use]
    pub const fn requested_output_tokens(&self) -> Option<u64> {
        self.requested_output_tokens
    }
}

/// 同一客户端连接内由 Provider 生成并解释的不透明会话状态。
///
/// Core 只在重试和路由过程中保持该值；协议层只能把 Provider 返回的状态原样带入
/// 下一轮，不能读取或改写正文。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSessionState {
    provider: String,
    payload: Map<String, Value>,
}

impl ProviderSessionState {
    /// 创建 Provider 私有会话状态。
    ///
    /// # Errors
    ///
    /// Provider 名称无效时返回错误。
    pub fn new(
        provider: impl Into<String>,
        payload: Map<String, Value>,
    ) -> Result<Self, OperationError> {
        let provider = provider.into();
        validate_text(&provider, 64, true, None).map_err(|_| OperationError::EmptyField {
            field: "provider_session_state provider",
        })?;
        Ok(Self { provider, payload })
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub const fn payload(&self) -> &Map<String, Value> {
        &self.payload
    }
}

impl fmt::Debug for ProviderSessionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSessionState")
            .field("provider", &self.provider)
            .field("payload", &"<not included in Debug>")
            .finish()
    }
}

/// 客户端协议交给 Provider 的不透明 JSON object。
///
/// Core 只负责在路由和重试过程中保持该值，不读取或改写正文。协议 adapter
/// 与对应 Provider 共同拥有其语义。
#[derive(Clone, PartialEq)]
pub struct ProtocolPayload {
    protocol: String,
    body: Map<String, Value>,
    context: Map<String, Value>,
}

impl ProtocolPayload {
    /// 创建协议不透明正文。
    ///
    /// # Errors
    ///
    /// 协议名称为空、过长或含控制字符时返回错误。
    pub fn json_object(
        protocol: impl Into<String>,
        body: Map<String, Value>,
    ) -> Result<Self, OperationError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None).map_err(|_| OperationError::EmptyField {
            field: "protocol_payload protocol",
        })?;
        Ok(Self {
            protocol,
            body,
            context: Map::new(),
        })
    }

    /// 返回协议名称。
    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// 返回仅供对应 Provider 解释的 JSON object。
    #[must_use]
    pub const fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    /// 附着不写入 wire body 的协议连接上下文。
    ///
    /// Core 只保留该值；对应 Provider 可以读取已知键，未知键必须忽略。
    #[must_use]
    pub fn with_context(mut self, context: Map<String, Value>) -> Self {
        self.context = context;
        self
    }

    /// 返回仅供对应 Provider 解释的非 wire 上下文。
    #[must_use]
    pub const fn context(&self) -> &Map<String, Value> {
        &self.context
    }
}

impl fmt::Debug for ProtocolPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtocolPayload")
            .field("protocol", &self.protocol)
            .field("body", &"<not included in Debug>")
            .field("context", &"<not included in Debug>")
            .finish()
    }
}

/// 协议 adapter 与 Provider 之间不透明传递的 JSON 正文。
///
/// Core 保留字节与非 wire 上下文，不验证、解析或改写正文；
/// 请求载荷与原生模型目录均由协议 adapter 和对应 Provider 共同拥有语义。
#[derive(Clone, PartialEq, Eq)]
pub struct RawJsonPayload {
    protocol: String,
    body: Bytes,
    context: Map<String, Value>,
    translated: bool,
}

impl RawJsonPayload {
    /// 创建不透明的协议 JSON 正文。
    ///
    /// # Errors
    ///
    /// 协议名称为空、过长或含控制字符时返回错误。
    pub fn new(protocol: impl Into<String>, body: Bytes) -> Result<Self, OperationError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None).map_err(|_| OperationError::EmptyField {
            field: "raw_json_payload protocol",
        })?;
        Ok(Self {
            protocol,
            body,
            context: Map::new(),
            translated: false,
        })
    }

    /// 返回协议名称。
    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// 返回协议正文的 JSON 字节。
    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }

    /// 附着不写入 wire body 的协议连接上下文。
    #[must_use]
    pub fn with_context(mut self, context: Map<String, Value>) -> Self {
        self.context = context;
        self
    }

    /// 返回仅供对应 Provider 解释的非 wire 上下文。
    #[must_use]
    pub const fn context(&self) -> &Map<String, Value> {
        &self.context
    }
}

impl fmt::Debug for RawJsonPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawJsonPayload")
            .field("protocol", &self.protocol)
            .field("body", &"<not included in Debug>")
            .field("context", &"<not included in Debug>")
            .finish()
    }
}

/// Provider HTTP 端点使用的不透明字节正文。
///
/// 与 [`RawJsonPayload`] 不同，这里不要求正文是 JSON。Core 只保留字节和
/// 非 wire 上下文，最终 origin、路径与账号认证仍由 Provider 决定。
#[derive(Clone, PartialEq, Eq)]
pub struct RawHttpPayload {
    protocol: String,
    body: Bytes,
    context: Map<String, Value>,
    translated: bool,
}

impl RawHttpPayload {
    /// 创建不透明 HTTP 正文。
    ///
    /// # Errors
    ///
    /// 协议名称为空、过长或含控制字符时返回错误。
    pub fn new(protocol: impl Into<String>, body: Bytes) -> Result<Self, OperationError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None).map_err(|_| OperationError::EmptyField {
            field: "raw_http_payload protocol",
        })?;
        Ok(Self {
            protocol,
            body,
            context: Map::new(),
            translated: false,
        })
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }

    #[must_use]
    pub fn with_context(mut self, context: Map<String, Value>) -> Self {
        self.context = context;
        self
    }

    #[must_use]
    pub const fn context(&self) -> &Map<String, Value> {
        &self.context
    }
}

impl fmt::Debug for RawHttpPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawHttpPayload")
            .field("protocol", &self.protocol)
            .field("body", &"<not included in Debug>")
            .field("context", &"<not included in Debug>")
            .finish()
    }
}

/// 通用生成请求。
#[derive(Clone, PartialEq)]
pub struct GenerateRequest {
    payload: Arc<GeneratePayload>,
}

#[derive(Clone, PartialEq)]
struct GeneratePayload {
    protocol_payload: ProtocolPayload,
    provider_session_state: Option<ProviderSessionState>,
    source_requirements: Option<CapabilityRequirements>,
    middleware_requirements: Option<(CapabilityRequirements, CapabilityRequirements)>,
}

impl GenerateRequest {
    /// 创建携带协议不透明正文的生成请求。
    ///
    /// 完整请求只以原始 wire object 保存；Core 仅局部只读已知字段以推导路由能力，
    /// Provider 负责解释与转换正文。
    #[must_use]
    pub fn from_protocol_payload(protocol_payload: ProtocolPayload) -> Self {
        Self {
            payload: Arc::new(GeneratePayload {
                protocol_payload,
                provider_session_state: None,
                source_requirements: None,
                middleware_requirements: None,
            }),
        }
    }

    /// 写回原生 Provider 编码结果，保留已准入的能力、连接上下文和会话状态。
    ///
    /// 原生编码不消耗中间件的一次跨协议改写额度：可以保留协议名，也可以在一次
    /// 中间件改写后编码到 Provider wire。调用方仍须验证保护字段没有被改动。
    ///
    /// # Errors
    ///
    /// 目标协议名无效时返回错误。
    pub fn with_native_encoded_body(
        mut self,
        protocol: impl Into<String>,
        body: Map<String, Value>,
    ) -> Result<Self, OperationError> {
        let encoded = ProtocolPayload::json_object(protocol, body)?
            .with_context(self.protocol_payload().context().clone());
        let requirements = self.requirements();
        let payload = Arc::make_mut(&mut self.payload);
        payload.protocol_payload = encoded;
        payload.source_requirements = Some(requirements);
        Ok(self)
    }

    /// 附着同一客户端连接上一轮由 Provider 返回的不透明状态。
    #[must_use]
    pub fn with_provider_session_state(mut self, state: ProviderSessionState) -> Self {
        self.set_provider_session_state(state);
        self
    }

    /// 原地附着会话状态；payload 独占时不复制正文。
    pub fn set_provider_session_state(&mut self, state: ProviderSessionState) {
        Arc::make_mut(&mut self.payload).provider_session_state = Some(state);
    }

    /// 返回最大输出 token 数。
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u64> {
        self.body()
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0)
    }

    /// 返回客户端提供的 prompt cache 路由键。
    #[must_use]
    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.body()
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty())
    }

    /// 返回原始请求是否要求 Provider 原生 continuation。
    #[must_use]
    pub fn native_continuation_requested(&self) -> bool {
        self.body()
            .get("previous_response_id")
            .and_then(Value::as_str)
            .is_some()
    }

    /// 返回客户端是否请求了图片生成工具。
    #[must_use]
    pub fn image_generation_requested(&self) -> bool {
        self.body()
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools.iter().any(|tool| {
                    tool.get("type").and_then(Value::as_str) == Some("image_generation")
                })
            })
    }

    /// 返回指定 Provider 的连接内会话状态。
    #[must_use]
    pub fn provider_session_state(&self, provider: &str) -> Option<&ProviderSessionState> {
        self.payload
            .provider_session_state
            .as_ref()
            .filter(|state| state.provider() == provider)
    }

    /// 返回协议不透明正文。
    #[must_use]
    pub fn protocol_payload(&self) -> &ProtocolPayload {
        &self.payload.protocol_payload
    }

    fn requirements(&self) -> CapabilityRequirements {
        if let Some(requirements) = &self.payload.source_requirements {
            return requirements.clone();
        }
        let mut requirements = CapabilityRequirements::new(OperationKind::Generate)
            .with_requested_output_tokens(self.max_output_tokens());
        if let Some((_, inherited)) = &self.payload.middleware_requirements {
            for feature in inherited.features() {
                requirements = requirements.require(*feature);
            }
        }
        if self
            .body()
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
        {
            requirements = requirements.require(Feature::Tools);
        }
        if contains_type(self.body().get("input"), "input_image") {
            requirements = requirements.require(Feature::Vision);
        }
        if self.body().get("reasoning").is_some_and(Value::is_object) {
            requirements = requirements.require(Feature::Reasoning);
        }
        if self
            .body()
            .get("text")
            .and_then(Value::as_object)
            .and_then(|text| text.get("format"))
            .and_then(Value::as_object)
            .and_then(|format| format.get("type"))
            .and_then(Value::as_str)
            == Some("json_schema")
        {
            requirements = requirements.require(Feature::JsonSchema);
        }
        if self.native_continuation_requested() {
            requirements.require(Feature::NativeContinuation)
        } else {
            requirements
        }
    }

    fn body(&self) -> &Map<String, Value> {
        self.protocol_payload().body()
    }
}

impl fmt::Debug for GenerateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerateRequest")
            .field(
                "provider_session_state",
                &self
                    .payload
                    .provider_session_state
                    .as_ref()
                    .map(|_| "<present>"),
            )
            .field("protocol_payload", self.protocol_payload())
            .finish()
    }
}

fn contains_type(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .any(|value| contains_type(Some(value), expected)),
        Some(Value::Object(object)) => {
            object.get("type").and_then(Value::as_str) == Some(expected)
                || object
                    .values()
                    .any(|value| contains_type(Some(value), expected))
        }
        _ => false,
    }
}

/// 图像 API 的稳定端点语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageRequestKind {
    /// 从文本生成图像。
    Generation,
    /// 使用输入图像执行编辑。
    Edit,
}

/// 图像生成或编辑请求。
#[derive(Clone, PartialEq)]
pub struct ImageRequest {
    kind: ImageRequestKind,
    payload: RawJsonPayload,
}

impl ImageRequest {
    /// 创建携带原始协议 JSON 正文的图像请求。
    #[must_use]
    pub const fn from_raw_json(kind: ImageRequestKind, payload: RawJsonPayload) -> Self {
        Self { kind, payload }
    }

    /// 返回图像 API 端点语义。
    #[must_use]
    pub const fn kind(&self) -> ImageRequestKind {
        self.kind
    }

    /// 返回未经重编码的协议 JSON 正文。
    #[must_use]
    pub const fn payload(&self) -> &RawJsonPayload {
        &self.payload
    }
}

impl fmt::Debug for ImageRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageRequest")
            .field("kind", &self.kind)
            .field("payload", &"<not included in Debug>")
            .finish()
    }
}

/// Provider 原生 standalone search 请求。
#[derive(Clone, PartialEq, Eq)]
pub struct StandaloneSearchRequest {
    payload: RawJsonPayload,
}

impl StandaloneSearchRequest {
    /// 创建携带原始协议 JSON 正文的 standalone search 请求。
    #[must_use]
    pub const fn from_raw_json(payload: RawJsonPayload) -> Self {
        Self { payload }
    }

    /// 返回未经重编码的协议 JSON 正文。
    #[must_use]
    pub const fn payload(&self) -> &RawJsonPayload {
        &self.payload
    }
}

impl fmt::Debug for StandaloneSearchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StandaloneSearchRequest")
            .field("payload", &"<not included in Debug>")
            .finish()
    }
}

/// 使用目标模型的 Provider 原生 Token 计数请求。
#[derive(Clone, PartialEq, Eq)]
pub struct TokenCountRequest {
    payload: RawJsonPayload,
}

impl TokenCountRequest {
    #[must_use]
    pub const fn from_raw_json(payload: RawJsonPayload) -> Self {
        Self { payload }
    }

    #[must_use]
    pub const fn payload(&self) -> &RawJsonPayload {
        &self.payload
    }
}

impl fmt::Debug for TokenCountRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenCountRequest")
            .field("payload", &"<not included in Debug>")
            .finish()
    }
}

/// API 可公开的 Provider HTTP method 集合；不自动包含 `HEAD` 或任意 method。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderHttpMethod {
    Get,
    Post,
}

impl ProviderHttpMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// 已由 API 解析的单个 HTTP header；值不会出现在 `Debug`。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderHttpHeader {
    name: String,
    value: Bytes,
}

impl ProviderHttpHeader {
    #[must_use]
    pub fn new(name: impl Into<String>, value: Bytes) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn value(&self) -> &Bytes {
        &self.value
    }
}

impl fmt::Debug for ProviderHttpHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderHttpHeader")
            .field("name", &self.name)
            .field("value", &"<not included in Debug>")
            .finish()
    }
}

/// Provider 显式声明的账号认证 HTTP 操作。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderHttpRequest {
    endpoint: String,
    method: ProviderHttpMethod,
    query: Option<String>,
    headers: Vec<ProviderHttpHeader>,
    payload: RawHttpPayload,
}

impl ProviderHttpRequest {
    /// # Errors
    ///
    /// endpoint 不是安全符号名，query 或 headers 超过边界时返回错误。
    pub fn new(
        endpoint: impl Into<String>,
        method: ProviderHttpMethod,
        query: Option<String>,
        headers: Vec<ProviderHttpHeader>,
        payload: RawHttpPayload,
    ) -> Result<Self, OperationError> {
        let endpoint = endpoint.into();
        let valid_endpoint = !endpoint.is_empty()
            && endpoint.len() <= 64
            && endpoint != "."
            && endpoint != ".."
            && endpoint
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        let valid_query = query.as_ref().is_none_or(|query| {
            query.len() <= 8 * 1024 && !query.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
        });
        let valid_headers = headers.len() <= 64
            && headers
                .iter()
                .try_fold(0_usize, |total, header| {
                    let valid_name = !header.name.is_empty()
                        && header.name.len() <= 128
                        && header
                            .name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
                    let valid_value = header.value.len() <= 8 * 1024
                        && !header
                            .value
                            .iter()
                            .any(|byte| matches!(byte, b'\r' | b'\n' | 0));
                    valid_name
                        .then_some(())
                        .filter(|_| valid_value)
                        .and_then(|()| total.checked_add(header.name.len() + header.value.len()))
                        .filter(|total| *total <= 32 * 1024)
                })
                .is_some();
        if !valid_endpoint || !valid_query || !valid_headers {
            return Err(OperationError::EmptyField {
                field: "provider_http_request",
            });
        }
        Ok(Self {
            endpoint,
            method,
            query,
            headers,
            payload,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    #[must_use]
    pub const fn method(&self) -> ProviderHttpMethod {
        self.method
    }

    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    #[must_use]
    pub fn headers(&self) -> &[ProviderHttpHeader] {
        &self.headers
    }

    #[must_use]
    pub const fn payload(&self) -> &RawHttpPayload {
        &self.payload
    }
}

impl fmt::Debug for ProviderHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderHttpRequest")
            .field("endpoint", &self.endpoint)
            .field("method", &self.method)
            .field("query", &self.query.as_ref().map(|_| "<present>"))
            .field("header_count", &self.headers.len())
            .field("payload", &self.payload)
            .finish()
    }
}

/// 网关内部业务请求；不包含任何客户端 wire 或 Provider SDK 类型。
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum Operation {
    /// 生成。
    Generate(GenerateRequest),
    /// 图像生成。
    GenerateImage(ImageRequest),
    /// Provider 原生 standalone search。
    Search(StandaloneSearchRequest),
    /// 使用目标模型进行真实 Token 计数。
    CountTokens(TokenCountRequest),
    /// Provider 显式登记的账号认证 HTTP 操作。
    ProviderHttp(ProviderHttpRequest),
}

impl Operation {
    pub(crate) fn with_middleware_requirements(
        self,
        original: CapabilityRequirements,
        inherited: CapabilityRequirements,
    ) -> Result<Self, OperationError> {
        let Self::Generate(mut request) = self else {
            return Err(OperationError::EmptyField {
                field: "middleware capabilities",
            });
        };
        Arc::make_mut(&mut request.payload).middleware_requirements = Some((original, inherited));
        Ok(Self::Generate(request))
    }

    /// 请求转换前的需求仅供诊断，选路始终使用有效上游需求。
    #[must_use]
    pub fn original_capability_requirements(&self) -> CapabilityRequirements {
        if let Self::Generate(request) = self
            && let Some((original, _)) = &request.payload.middleware_requirements
        {
            return original.clone();
        }
        self.capability_requirements()
    }

    /// 把协议正文编码为中间件可见的原始字节；不包含非 wire context 或会话状态。
    ///
    /// # Errors
    ///
    /// 生成请求的 JSON object 无法编码时返回错误。
    pub fn middleware_body(&self) -> Result<Bytes, OperationError> {
        match self {
            Self::Generate(request) => serde_json::to_vec(request.protocol_payload().body())
                .map(Bytes::from)
                .map_err(|_| OperationError::EmptyField {
                    field: "protocol_payload body",
                }),
            Self::GenerateImage(request) => Ok(request.payload().body().clone()),
            Self::Search(request) => Ok(request.payload().body().clone()),
            Self::CountTokens(request) => Ok(request.payload().body().clone()),
            Self::ProviderHttp(request) => Ok(request.payload().body().clone()),
        }
    }

    /// 写回中间件返回的协议与正文，同时保留宿主持有的非 wire context。
    ///
    /// 同协议是正文替换；跨协议只能使用既有 direct-once 转换边界，不能借中间件
    /// 绕过重复转换保护。
    ///
    /// # Errors
    ///
    /// 协议名或正文无效，或请求已经发生过一次跨协议转换时返回错误。
    pub fn replace_middleware_wire(
        self,
        protocol: impl Into<String>,
        body: Bytes,
    ) -> Result<Self, OperationError> {
        let protocol = protocol.into();
        if self.protocol() != protocol {
            return self.translate_protocol_wire(protocol, body);
        }
        let context = match &self {
            Self::Generate(request) => request.protocol_payload().context().clone(),
            Self::GenerateImage(request) => request.payload().context().clone(),
            Self::Search(request) => request.payload().context().clone(),
            Self::CountTokens(request) => request.payload().context().clone(),
            Self::ProviderHttp(request) => request.payload().context().clone(),
        };
        self.replace_protocol_wire(Some(body), context)
    }

    /// 替换协议正文和非 wire 上下文，同时保留 operation 类别、Provider 会话状态与
    /// 原协议名称。`body=None` 表示只改上下文，避免无加工路径重新编码正文。
    ///
    /// # Errors
    ///
    /// Generate 的替换正文不是 JSON object 时返回错误；原始 JSON 端点继续按字节保存。
    pub fn replace_protocol_wire(
        self,
        body: Option<Bytes>,
        context: Map<String, Value>,
    ) -> Result<Self, OperationError> {
        self.replace_protocol_wire_inner(None, body, context)
    }

    /// 替换 Provider HTTP 正文、非 wire 上下文和已解析 headers，同时保持 endpoint、
    /// method 与 query 不变。headers 继续由 [`ProviderHttpRequest::new`] 统一验证；
    /// Core 不解析插件传输使用的 base64 表示。
    ///
    /// # Errors
    ///
    /// 当前 operation 不是 Provider HTTP，或替换后的 headers 超出请求边界时返回错误。
    pub fn replace_provider_http_wire(
        self,
        body: Option<Bytes>,
        context: Map<String, Value>,
        headers: Vec<ProviderHttpHeader>,
    ) -> Result<Self, OperationError> {
        let Self::ProviderHttp(request) = self else {
            return Err(OperationError::EmptyField {
                field: "provider_http_request",
            });
        };
        let payload = request.payload;
        ProviderHttpRequest::new(
            request.endpoint,
            request.method,
            request.query,
            headers,
            RawHttpPayload {
                protocol: payload.protocol,
                body: body.unwrap_or(payload.body),
                context,
                translated: payload.translated,
            },
        )
        .map(Self::ProviderHttp)
    }

    /// 把一次已选择的协议转换结果写回 operation，同时保留非 wire 上下文和
    /// Provider 会话状态。该方法只验证公共载荷形状，不解释目标协议字段。
    ///
    /// # Errors
    ///
    /// 目标协议名无效，或 Generate 的目标正文不是 JSON object 时返回错误。
    pub fn translate_protocol_wire(
        self,
        target_protocol: impl Into<String>,
        body: Bytes,
    ) -> Result<Self, OperationError> {
        let target_protocol = target_protocol.into();
        validate_text(&target_protocol, 64, true, None).map_err(|_| {
            OperationError::EmptyField {
                field: "translated protocol",
            }
        })?;
        if self.protocol() == target_protocol || self.protocol_was_translated() {
            return Err(OperationError::EmptyField {
                field: "direct protocol translation",
            });
        }
        let context = match &self {
            Self::Generate(request) => request.protocol_payload().context().clone(),
            Self::GenerateImage(request) => request.payload().context().clone(),
            Self::Search(request) => request.payload().context().clone(),
            Self::CountTokens(request) => request.payload().context().clone(),
            Self::ProviderHttp(request) => request.payload().context().clone(),
        };
        self.replace_protocol_wire_inner(Some(target_protocol), Some(body), context)
    }

    fn protocol_was_translated(&self) -> bool {
        match self {
            Self::Generate(request) => request.payload.source_requirements.is_some(),
            Self::GenerateImage(request) => request.payload().translated,
            Self::Search(request) => request.payload().translated,
            Self::CountTokens(request) => request.payload().translated,
            Self::ProviderHttp(request) => request.payload().translated,
        }
    }

    fn replace_protocol_wire_inner(
        self,
        target_protocol: Option<String>,
        body: Option<Bytes>,
        context: Map<String, Value>,
    ) -> Result<Self, OperationError> {
        let translating = target_protocol.is_some();
        match self {
            Self::Generate(request) => {
                let protocol_payload = request.protocol_payload();
                let source_requirements = translating.then(|| request.requirements());
                let body = match body {
                    Some(body) => {
                        serde_json::from_slice::<Map<String, Value>>(&body).map_err(|_| {
                            OperationError::EmptyField {
                                field: "protocol_payload body",
                            }
                        })?
                    }
                    None => protocol_payload.body().clone(),
                };
                let replacement = ProtocolPayload {
                    protocol: target_protocol
                        .unwrap_or_else(|| protocol_payload.protocol().to_owned()),
                    body,
                    context,
                };
                let mut payload = (*request.payload).clone();
                payload.protocol_payload = replacement;
                if let Some(requirements) = source_requirements {
                    payload.source_requirements = Some(requirements);
                }
                Ok(Self::Generate(GenerateRequest {
                    payload: Arc::new(payload),
                }))
            }
            Self::GenerateImage(request) => {
                let payload = request.payload;
                Ok(Self::GenerateImage(ImageRequest {
                    kind: request.kind,
                    payload: RawJsonPayload {
                        protocol: target_protocol.unwrap_or(payload.protocol),
                        body: body.unwrap_or(payload.body),
                        context,
                        translated: payload.translated || translating,
                    },
                }))
            }
            Self::Search(request) => {
                let payload = request.payload;
                Ok(Self::Search(StandaloneSearchRequest {
                    payload: RawJsonPayload {
                        protocol: target_protocol.unwrap_or(payload.protocol),
                        body: body.unwrap_or(payload.body),
                        context,
                        translated: payload.translated || translating,
                    },
                }))
            }
            Self::CountTokens(request) => {
                let payload = request.payload;
                Ok(Self::CountTokens(TokenCountRequest {
                    payload: RawJsonPayload {
                        protocol: target_protocol.unwrap_or(payload.protocol),
                        body: body.unwrap_or(payload.body),
                        context,
                        translated: payload.translated || translating,
                    },
                }))
            }
            Self::ProviderHttp(request) => {
                let payload = request.payload;
                Ok(Self::ProviderHttp(ProviderHttpRequest {
                    endpoint: request.endpoint,
                    method: request.method,
                    query: request.query,
                    headers: request.headers,
                    payload: RawHttpPayload {
                        protocol: target_protocol.unwrap_or(payload.protocol),
                        body: body.unwrap_or(payload.body),
                        context,
                        translated: payload.translated || translating,
                    },
                }))
            }
        }
    }

    /// 返回协议 adapter 持有的不透明格式标识。
    #[must_use]
    pub fn protocol(&self) -> &str {
        match self {
            Self::Generate(request) => request.protocol_payload().protocol(),
            Self::GenerateImage(request) => request.payload().protocol(),
            Self::Search(request) => request.payload().protocol(),
            Self::CountTokens(request) => request.payload().protocol(),
            Self::ProviderHttp(request) => request.payload().protocol(),
        }
    }

    /// 附着协议连接持有的 Provider 私有状态；非生成 operation 保持不变。
    #[must_use]
    pub fn with_provider_session_state(self, state: ProviderSessionState) -> Self {
        match self {
            Self::Generate(request) => Self::Generate(request.with_provider_session_state(state)),
            operation => operation,
        }
    }

    /// 原地附着 Provider 私有状态；payload 独占时不复制正文。
    pub fn set_provider_session_state(&mut self, state: ProviderSessionState) {
        if let Self::Generate(request) = self {
            request.set_provider_session_state(state);
        }
    }

    /// 返回稳定 operation 分类。
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::Generate(_) => OperationKind::Generate,
            Self::GenerateImage(_) => OperationKind::GenerateImage,
            Self::Search(_) => OperationKind::Search,
            Self::CountTokens(_) => OperationKind::CountTokens,
            Self::ProviderHttp(_) => OperationKind::ProviderHttp,
        }
    }

    /// 推导 Router 使用的能力要求。
    #[must_use]
    pub fn capability_requirements(&self) -> CapabilityRequirements {
        match self {
            Self::Generate(request) => request.requirements(),
            Self::GenerateImage(_) => CapabilityRequirements::new(OperationKind::GenerateImage),
            Self::Search(_) => CapabilityRequirements::new(OperationKind::Search),
            Self::CountTokens(_) => CapabilityRequirements::new(OperationKind::CountTokens),
            Self::ProviderHttp(_) => CapabilityRequirements::new(OperationKind::ProviderHttp),
        }
    }

    /// 返回该 operation 是否代表一次图片生成请求。
    #[must_use]
    pub fn image_generation_requested(&self) -> bool {
        match self {
            Self::Generate(request) => request.image_generation_requested(),
            Self::GenerateImage(_) => true,
            Self::Search(_) | Self::CountTokens(_) | Self::ProviderHttp(_) => false,
        }
    }

    /// 返回当前 Provider 的连接内私有状态。
    #[must_use]
    pub fn provider_session_state(&self, provider: &str) -> Option<&ProviderSessionState> {
        match self {
            Self::Generate(request) => request.provider_session_state(provider),
            Self::GenerateImage(_)
            | Self::Search(_)
            | Self::CountTokens(_)
            | Self::ProviderHttp(_) => None,
        }
    }
}

impl fmt::Debug for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Operation")
            .field("kind", &self.kind())
            .field("payload", &"<not included in Debug>")
            .finish()
    }
}
