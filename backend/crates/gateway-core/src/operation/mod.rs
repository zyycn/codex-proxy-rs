//! 协议无关的业务 operation。
//!
//! 这里只保留网关需要解释、路由或结算的稳定语义。Provider 专属字段被
//! 限制在按 Provider 命名的 [`ProviderOptions`] 中。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value};

use crate::error::{OperationError, validate_text};

/// 网关支持的稳定 operation 分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum OperationKind {
    /// 文本、多模态和工具生成。
    Generate,
    /// 向量嵌入。
    Embed,
    /// 文档重排。
    Rerank,
    /// 图像生成。
    GenerateImage,
    /// 语音生成。
    Speech,
}

impl OperationKind {
    /// 返回注册表和持久化使用的稳定名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::Embed => "embed",
            Self::Rerank => "rerank",
            Self::GenerateImage => "generate_image",
            Self::Speech => "speech",
        }
    }
}

/// 请求在 commit 前是否允许跨目标重放。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetrySafety {
    /// 相同业务 payload 可以安全重放。
    Idempotent,
    /// 默认禁止跨 Provider fallback。
    NonIdempotent,
}

/// 客户端对生成结果的持久化意图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResponsePersistence {
    /// 允许所选 Provider 保存响应状态。
    Store,
    /// 要求所选 Provider 不保存响应状态。
    DoNotStore,
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
    minimum_context_tokens: u64,
    requested_output_tokens: Option<u64>,
}

impl CapabilityRequirements {
    /// 创建仅要求 operation 的能力约束。
    #[must_use]
    pub fn new(operation: OperationKind) -> Self {
        Self {
            operation,
            features: BTreeSet::new(),
            minimum_context_tokens: 0,
            requested_output_tokens: None,
        }
    }

    /// 增加稳定能力要求。
    #[must_use]
    pub fn require(mut self, feature: Feature) -> Self {
        self.features.insert(feature);
        self
    }

    /// 设置估算的最小 context token 数。
    #[must_use]
    pub const fn with_minimum_context_tokens(mut self, tokens: u64) -> Self {
        self.minimum_context_tokens = tokens;
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

    /// 返回估算的最小 context token 数。
    #[must_use]
    pub const fn minimum_context_tokens(&self) -> u64 {
        self.minimum_context_tokens
    }

    /// 返回请求的最大输出 token 数。
    #[must_use]
    pub const fn requested_output_tokens(&self) -> Option<u64> {
        self.requested_output_tokens
    }
}

/// 按 Provider 命名的专属参数。
#[derive(Clone, Default, PartialEq)]
pub struct ProviderOptions(BTreeMap<String, Map<String, Value>>);

impl ProviderOptions {
    /// 创建空 Provider 参数集合。
    #[must_use]
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// 插入由对应 Provider adapter 独占校验的 JSON object。
    ///
    /// # Errors
    ///
    /// Provider 名称无效或同名参数已经存在时返回错误。
    pub fn insert(
        &mut self,
        provider: impl Into<String>,
        options: Map<String, Value>,
    ) -> Result<(), OperationError> {
        let provider = provider.into();
        validate_text(&provider, 64, true, None).map_err(|_| OperationError::EmptyField {
            field: "provider_options provider",
        })?;
        if self.0.contains_key(&provider) {
            return Err(OperationError::DuplicateProviderOptions { provider });
        }
        self.0.insert(provider, options);
        Ok(())
    }

    /// 返回某个 Provider 的参数。
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<&Map<String, Value>> {
        self.0.get(provider)
    }

    /// 返回所有声明了参数的 Provider 名称。
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// 判断集合是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ProviderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderOptions")
            .field("providers", &self.0.keys().collect::<Vec<_>>())
            .field("values", &"<not included in Debug>")
            .finish()
    }
}

/// 生成输入的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageRole {
    /// 系统指令。
    System,
    /// 开发者指令；与系统角色分开保留，避免协议 adapter 丢失优先级语义。
    Developer,
    /// 用户输入。
    User,
    /// 助手历史输出。
    Assistant,
}

/// 图像输入来源。
#[derive(Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// 外部 URL；协议 adapter 必须先完成长度与 scheme 校验。
    Url(String),
    /// 已解码的二进制图像。
    Bytes {
        /// MIME type。
        media_type: String,
        /// 图像内容。
        data: Vec<u8>,
    },
}

impl fmt::Debug for ImageSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(_) => formatter.write_str("ImageSource::Url(<redacted>)"),
            Self::Bytes { media_type, data } => formatter
                .debug_struct("ImageSource::Bytes")
                .field("media_type", media_type)
                .field("bytes", &data.len())
                .finish(),
        }
    }
}

/// 生成输入的稳定内容片段。
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentPart {
    /// 文本内容。
    Text(String),
    /// 图像内容。
    Image(ImageSource),
    /// 工具结果。
    ToolResult {
        /// 对应 tool call ID。
        call_id: String,
        /// 工具输出文本。
        output: String,
    },
    /// 助手发起的工具调用；portable history 用它在跨 Provider 时恢复调用上下文。
    ToolCall {
        /// 稳定 tool call ID。
        call_id: String,
        /// 工具名称。
        name: String,
        /// 完整 JSON arguments 文本。
        arguments: String,
    },
}

impl fmt::Debug for ContentPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => formatter
                .debug_tuple("Text")
                .field(&format_args!("<{} bytes>", text.len()))
                .finish(),
            Self::Image(source) => formatter.debug_tuple("Image").field(source).finish(),
            Self::ToolResult { call_id, output } => formatter
                .debug_struct("ToolResult")
                .field("call_id", call_id)
                .field("output", &format_args!("<{} bytes>", output.len()))
                .finish(),
            Self::ToolCall {
                call_id,
                name,
                arguments,
            } => formatter
                .debug_struct("ToolCall")
                .field("call_id", call_id)
                .field("name", name)
                .field("arguments", &format_args!("<{} bytes>", arguments.len()))
                .finish(),
        }
    }
}

/// 一项有角色的生成输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    role: MessageRole,
    content: Vec<ContentPart>,
}

impl Message {
    /// 创建消息。
    ///
    /// # Errors
    ///
    /// 内容为空时返回错误。
    pub fn new(role: MessageRole, content: Vec<ContentPart>) -> Result<Self, OperationError> {
        if content.is_empty() {
            return Err(OperationError::EmptyField { field: "content" });
        }
        Ok(Self { role, content })
    }

    /// 返回消息角色。
    #[must_use]
    pub const fn role(&self) -> MessageRole {
        self.role
    }

    /// 返回内容片段。
    #[must_use]
    pub fn content(&self) -> &[ContentPart] {
        &self.content
    }
}

/// 工具声明。
#[derive(Clone, PartialEq)]
pub struct ToolDefinition {
    name: String,
    description: Option<String>,
    input_schema: Map<String, Value>,
    strict: bool,
}

impl ToolDefinition {
    /// 创建工具声明。
    ///
    /// # Errors
    ///
    /// 名称为空时返回错误。
    pub fn new(
        name: impl Into<String>,
        description: Option<String>,
        input_schema: Map<String, Value>,
    ) -> Result<Self, OperationError> {
        let name = name.into();
        if name.is_empty() {
            return Err(OperationError::EmptyField { field: "tool.name" });
        }
        Ok(Self {
            name,
            description,
            input_schema,
            strict: false,
        })
    }

    /// 设置是否要求 Provider 严格遵守输入 schema。
    #[must_use]
    pub const fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// 返回工具名。
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回工具说明。
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// 返回工具输入 schema。
    #[must_use]
    pub const fn input_schema(&self) -> &Map<String, Value> {
        &self.input_schema
    }

    /// 返回是否启用严格 schema。
    #[must_use]
    pub const fn strict(&self) -> bool {
        self.strict
    }
}

impl fmt::Debug for ToolDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field(
                "description",
                &self.description.as_ref().map(|_| "<present>"),
            )
            .field("input_schema", &"<validated JSON object>")
            .field("strict", &self.strict)
            .finish()
    }
}

/// 结构化 JSON 输出的稳定约束。
#[derive(Clone, PartialEq)]
pub struct JsonSchemaFormat {
    name: String,
    description: Option<String>,
    schema: Map<String, Value>,
    strict: bool,
}

impl JsonSchemaFormat {
    /// 创建 JSON Schema 输出约束。
    ///
    /// # Errors
    ///
    /// Schema 名称为空时返回错误。
    pub fn new(
        name: impl Into<String>,
        description: Option<String>,
        schema: Map<String, Value>,
        strict: bool,
    ) -> Result<Self, OperationError> {
        let name = name.into();
        if name.is_empty() {
            return Err(OperationError::EmptyField {
                field: "text.format.name",
            });
        }
        Ok(Self {
            name,
            description,
            schema,
            strict,
        })
    }

    /// 返回 schema 名称。
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回可选说明。
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// 返回 JSON Schema object。
    #[must_use]
    pub const fn schema(&self) -> &Map<String, Value> {
        &self.schema
    }

    /// 返回是否要求严格结构化输出。
    #[must_use]
    pub const fn strict(&self) -> bool {
        self.strict
    }
}

impl fmt::Debug for JsonSchemaFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonSchemaFormat")
            .field("name", &self.name)
            .field(
                "description",
                &self.description.as_ref().map(|_| "<present>"),
            )
            .field("schema", &"<validated JSON object>")
            .field("strict", &self.strict)
            .finish()
    }
}

/// 生成输出格式。
#[derive(Clone, PartialEq)]
pub enum OutputFormat {
    /// 普通文本。
    Text,
    /// 任意 JSON object。
    JsonObject,
    /// 指定 JSON Schema。
    JsonSchema(JsonSchemaFormat),
}

impl fmt::Debug for OutputFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text => formatter.write_str("Text"),
            Self::JsonObject => formatter.write_str("JsonObject"),
            Self::JsonSchema(_) => formatter.write_str("JsonSchema(<validated>)"),
        }
    }
}

/// 推理强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasoningEffort {
    /// 最低推理预算。
    Minimal,
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
    /// Provider 支持时使用最高推理预算。
    ExtraHigh,
}

/// 可见推理摘要要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasoningSummary {
    /// 由模型选择摘要粒度。
    Auto,
    /// 简短摘要。
    Concise,
    /// 详细摘要。
    Detailed,
    /// 显式关闭摘要。
    None,
}

/// 推理要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReasoningRequirement {
    /// 推理强度。
    pub effort: Option<ReasoningEffort>,
    /// 可见推理摘要要求；`None` 表示客户端未指定。
    pub summary: Option<ReasoningSummary>,
}

/// 对话延续模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContinuationMode {
    /// Provider 原生绑定。
    Native,
}

/// 通用生成请求。
#[derive(Clone, PartialEq)]
pub struct GenerateRequest {
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    output_format: OutputFormat,
    reasoning: Option<ReasoningRequirement>,
    continuation: Option<ContinuationMode>,
    max_output_tokens: Option<u64>,
    estimated_context_tokens: u64,
    retry_safety: RetrySafety,
    response_persistence: ResponsePersistence,
    provider_options: ProviderOptions,
}

impl GenerateRequest {
    /// 创建最小生成请求。
    ///
    /// # Errors
    ///
    /// 消息为空时返回错误。
    pub fn new(messages: Vec<Message>) -> Result<Self, OperationError> {
        if messages.is_empty() {
            return Err(OperationError::EmptyField { field: "messages" });
        }
        Ok(Self {
            messages,
            tools: Vec::new(),
            output_format: OutputFormat::Text,
            reasoning: None,
            continuation: None,
            max_output_tokens: None,
            estimated_context_tokens: 0,
            retry_safety: RetrySafety::NonIdempotent,
            response_persistence: ResponsePersistence::Store,
            provider_options: ProviderOptions::new(),
        })
    }

    /// 设置工具。
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置输出格式。
    #[must_use]
    pub fn with_output_format(mut self, output_format: OutputFormat) -> Self {
        self.output_format = output_format;
        self
    }

    /// 设置推理要求。
    #[must_use]
    pub const fn with_reasoning(mut self, reasoning: ReasoningRequirement) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// 设置对话延续模式。
    #[must_use]
    pub const fn with_continuation(mut self, continuation: ContinuationMode) -> Self {
        self.continuation = Some(continuation);
        self
    }

    /// 设置输出 token 上限。
    #[must_use]
    pub const fn with_max_output_tokens(mut self, tokens: u64) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// 设置协议层估算的 context token 数。
    #[must_use]
    pub const fn with_estimated_context_tokens(mut self, tokens: u64) -> Self {
        self.estimated_context_tokens = tokens;
        self
    }

    /// 显式声明重放安全性。
    #[must_use]
    pub const fn with_retry_safety(mut self, retry_safety: RetrySafety) -> Self {
        self.retry_safety = retry_safety;
        self
    }

    /// 冻结客户端的响应持久化意图。
    #[must_use]
    pub const fn with_response_persistence(
        mut self,
        response_persistence: ResponsePersistence,
    ) -> Self {
        self.response_persistence = response_persistence;
        self
    }

    /// 设置 Provider 专属参数。
    #[must_use]
    pub fn with_provider_options(mut self, provider_options: ProviderOptions) -> Self {
        self.provider_options = provider_options;
        self
    }

    /// 返回消息。
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// 返回工具。
    #[must_use]
    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    /// 返回输出格式。
    #[must_use]
    pub const fn output_format(&self) -> &OutputFormat {
        &self.output_format
    }

    /// 返回推理要求。
    #[must_use]
    pub const fn reasoning(&self) -> Option<ReasoningRequirement> {
        self.reasoning
    }

    /// 返回对话延续模式。
    #[must_use]
    pub const fn continuation(&self) -> Option<ContinuationMode> {
        self.continuation
    }

    /// 返回最大输出 token 数。
    #[must_use]
    pub const fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// 返回协议层估算的 context token 数。
    #[must_use]
    pub const fn estimated_context_tokens(&self) -> u64 {
        self.estimated_context_tokens
    }

    /// 返回显式重放安全性。
    #[must_use]
    pub const fn retry_safety(&self) -> RetrySafety {
        self.retry_safety
    }

    /// 返回客户端的响应持久化意图。
    #[must_use]
    pub const fn response_persistence(&self) -> ResponsePersistence {
        self.response_persistence
    }

    /// 返回 Provider 专属参数。
    #[must_use]
    pub const fn provider_options(&self) -> &ProviderOptions {
        &self.provider_options
    }

    fn requirements(&self) -> CapabilityRequirements {
        let mut requirements = CapabilityRequirements::new(OperationKind::Generate)
            .with_minimum_context_tokens(self.estimated_context_tokens)
            .with_requested_output_tokens(self.max_output_tokens);
        if !self.tools.is_empty() {
            requirements = requirements.require(Feature::Tools);
        }
        if self.messages.iter().any(|message| {
            message
                .content()
                .iter()
                .any(|part| matches!(part, ContentPart::Image(_)))
        }) {
            requirements = requirements.require(Feature::Vision);
        }
        if self.reasoning.is_some() {
            requirements = requirements.require(Feature::Reasoning);
        }
        if matches!(self.output_format, OutputFormat::JsonSchema(_)) {
            requirements = requirements.require(Feature::JsonSchema);
        }
        if self.continuation.is_some() {
            requirements.require(Feature::NativeContinuation)
        } else {
            requirements
        }
    }
}

impl fmt::Debug for GenerateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerateRequest")
            .field("messages", &self.messages.len())
            .field("tools", &self.tools.len())
            .field("output_format", &self.output_format)
            .field("reasoning", &self.reasoning)
            .field("continuation", &self.continuation)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("estimated_context_tokens", &self.estimated_context_tokens)
            .field("retry_safety", &self.retry_safety)
            .field("response_persistence", &self.response_persistence)
            .field("provider_options", &self.provider_options)
            .finish()
    }
}

/// Embedding 请求。
#[derive(Clone, PartialEq, Eq)]
pub struct EmbedRequest {
    input: Vec<String>,
    dimensions: Option<u32>,
}

impl EmbedRequest {
    /// 创建 embedding 请求。
    ///
    /// # Errors
    ///
    /// 输入为空时返回错误。
    pub fn new(input: Vec<String>) -> Result<Self, OperationError> {
        if input.is_empty() || input.iter().any(String::is_empty) {
            return Err(OperationError::EmptyField { field: "input" });
        }
        Ok(Self {
            input,
            dimensions: None,
        })
    }

    /// 设置输出维度。
    #[must_use]
    pub const fn with_dimensions(mut self, dimensions: u32) -> Self {
        self.dimensions = Some(dimensions);
        self
    }

    /// 返回输入文本。
    #[must_use]
    pub fn input(&self) -> &[String] {
        &self.input
    }
}

impl fmt::Debug for EmbedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbedRequest")
            .field("input_items", &self.input.len())
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

/// Rerank 请求。
#[derive(Clone, PartialEq, Eq)]
pub struct RerankRequest {
    query: String,
    documents: Vec<String>,
    top_n: Option<u32>,
}

impl RerankRequest {
    /// 创建 rerank 请求。
    ///
    /// # Errors
    ///
    /// Query 或文档为空时返回错误。
    pub fn new(query: impl Into<String>, documents: Vec<String>) -> Result<Self, OperationError> {
        let query = query.into();
        if query.is_empty() {
            return Err(OperationError::EmptyField { field: "query" });
        }
        if documents.is_empty() || documents.iter().any(String::is_empty) {
            return Err(OperationError::EmptyField { field: "documents" });
        }
        Ok(Self {
            query,
            documents,
            top_n: None,
        })
    }

    /// 设置返回条数。
    #[must_use]
    pub const fn with_top_n(mut self, top_n: u32) -> Self {
        self.top_n = Some(top_n);
        self
    }
}

impl fmt::Debug for RerankRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RerankRequest")
            .field("query", &"<not included in Debug>")
            .field("documents", &self.documents.len())
            .field("top_n", &self.top_n)
            .finish()
    }
}

/// 图像生成请求。
#[derive(Clone, PartialEq, Eq)]
pub struct ImageRequest {
    prompt: String,
    count: u32,
    retry_safety: RetrySafety,
}

impl ImageRequest {
    /// 创建图像生成请求。
    ///
    /// # Errors
    ///
    /// Prompt 为空时返回错误。
    pub fn new(prompt: impl Into<String>) -> Result<Self, OperationError> {
        let prompt = prompt.into();
        if prompt.is_empty() {
            return Err(OperationError::EmptyField { field: "prompt" });
        }
        Ok(Self {
            prompt,
            count: 1,
            retry_safety: RetrySafety::NonIdempotent,
        })
    }

    /// 设置图像数量。
    #[must_use]
    pub const fn with_count(mut self, count: u32) -> Self {
        self.count = count;
        self
    }
}

impl fmt::Debug for ImageRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageRequest")
            .field("prompt", &"<not included in Debug>")
            .field("count", &self.count)
            .field("retry_safety", &self.retry_safety)
            .finish()
    }
}

/// 语音生成请求。
#[derive(Clone, PartialEq, Eq)]
pub struct SpeechRequest {
    input: String,
    voice: String,
    retry_safety: RetrySafety,
}

impl SpeechRequest {
    /// 创建语音生成请求。
    ///
    /// # Errors
    ///
    /// 输入或 voice 为空时返回错误。
    pub fn new(input: impl Into<String>, voice: impl Into<String>) -> Result<Self, OperationError> {
        let input = input.into();
        let voice = voice.into();
        if input.is_empty() {
            return Err(OperationError::EmptyField { field: "input" });
        }
        if voice.is_empty() {
            return Err(OperationError::EmptyField { field: "voice" });
        }
        Ok(Self {
            input,
            voice,
            retry_safety: RetrySafety::NonIdempotent,
        })
    }
}

impl fmt::Debug for SpeechRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpeechRequest")
            .field("input", &"<not included in Debug>")
            .field("voice", &self.voice)
            .field("retry_safety", &self.retry_safety)
            .finish()
    }
}

/// 网关内部业务请求；不包含任何客户端 wire 或 Provider SDK 类型。
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum Operation {
    /// 生成。
    Generate(GenerateRequest),
    /// Embedding。
    Embed(EmbedRequest),
    /// Rerank。
    Rerank(RerankRequest),
    /// 图像生成。
    GenerateImage(ImageRequest),
    /// 语音生成。
    Speech(SpeechRequest),
}

impl Operation {
    /// 返回稳定 operation 分类。
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::Generate(_) => OperationKind::Generate,
            Self::Embed(_) => OperationKind::Embed,
            Self::Rerank(_) => OperationKind::Rerank,
            Self::GenerateImage(_) => OperationKind::GenerateImage,
            Self::Speech(_) => OperationKind::Speech,
        }
    }

    /// 推导 Router 使用的能力要求。
    #[must_use]
    pub fn capability_requirements(&self) -> CapabilityRequirements {
        match self {
            Self::Generate(request) => request.requirements(),
            Self::Embed(_) => CapabilityRequirements::new(OperationKind::Embed),
            Self::Rerank(_) => CapabilityRequirements::new(OperationKind::Rerank),
            Self::GenerateImage(_) => CapabilityRequirements::new(OperationKind::GenerateImage),
            Self::Speech(_) => CapabilityRequirements::new(OperationKind::Speech),
        }
    }

    /// 返回跨目标重放安全性。
    #[must_use]
    pub const fn retry_safety(&self) -> RetrySafety {
        match self {
            Self::Generate(request) => request.retry_safety,
            Self::Embed(_) | Self::Rerank(_) => RetrySafety::Idempotent,
            Self::GenerateImage(request) => request.retry_safety,
            Self::Speech(request) => request.retry_safety,
        }
    }

    /// 返回当前 Provider 的专属请求参数。
    #[must_use]
    pub fn provider_options(&self, provider: &str) -> Option<&Map<String, Value>> {
        match self {
            Self::Generate(request) => request.provider_options().get(provider),
            Self::Embed(_) | Self::Rerank(_) | Self::GenerateImage(_) | Self::Speech(_) => None,
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
