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

use crate::error::{OperationError, validate_text};

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
}

impl OperationKind {
    /// 返回注册表和持久化使用的稳定名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::GenerateImage => "generate_image",
            Self::Search => "search",
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

/// 客户端协议交给 Provider 的未经重编码 JSON 正文。
///
/// 协议 adapter 只确定路由端点；Core 保留原始字节与非 wire 上下文，
/// 不验证、解析或改写正文。
#[derive(Clone, PartialEq, Eq)]
pub struct RawJsonPayload {
    protocol: String,
    body: Bytes,
    context: Map<String, Value>,
}

impl RawJsonPayload {
    /// 创建未经重编码的协议 JSON 正文。
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
        })
    }

    /// 返回协议名称。
    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// 返回 adapter 收到的原始 JSON 字节。
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

/// 通用生成请求。
#[derive(Clone, PartialEq)]
pub struct GenerateRequest {
    payload: Arc<GeneratePayload>,
}

#[derive(Clone, PartialEq)]
struct GeneratePayload {
    protocol_payload: ProtocolPayload,
    provider_session_state: Option<ProviderSessionState>,
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
            }),
        }
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
        let mut requirements = CapabilityRequirements::new(OperationKind::Generate)
            .with_requested_output_tokens(self.max_output_tokens());
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
}

impl Operation {
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
        }
    }

    /// 推导 Router 使用的能力要求。
    #[must_use]
    pub fn capability_requirements(&self) -> CapabilityRequirements {
        match self {
            Self::Generate(request) => request.requirements(),
            Self::GenerateImage(_) => CapabilityRequirements::new(OperationKind::GenerateImage),
            Self::Search(_) => CapabilityRequirements::new(OperationKind::Search),
        }
    }

    /// 返回该 operation 是否代表一次图片生成请求。
    #[must_use]
    pub fn image_generation_requested(&self) -> bool {
        match self {
            Self::Generate(request) => request.image_generation_requested(),
            Self::GenerateImage(_) => true,
            Self::Search(_) => false,
        }
    }

    /// 返回当前 Provider 的连接内私有状态。
    #[must_use]
    pub fn provider_session_state(&self, provider: &str) -> Option<&ProviderSessionState> {
        match self {
            Self::Generate(request) => request.provider_session_state(provider),
            Self::GenerateImage(_) | Self::Search(_) => None,
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
