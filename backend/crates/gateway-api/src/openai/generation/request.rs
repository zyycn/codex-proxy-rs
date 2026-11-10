//! 生成与计数入口共用的已解码操作和交付元数据

use gateway_core::operation::{Operation, ProviderSessionState};
use std::{fmt, net::IpAddr};

/// 客户端声明的 continuation 意图
#[derive(Clone, PartialEq, Eq)]
pub enum ContinuationIntent {
    /// 不使用先前响应
    None,
    /// 使用当前调用方可见的 OpenAI response ID
    PreviousResponseId(String),
}

impl ContinuationIntent {
    /// 返回待 history owner 解析的 response ID
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::PreviousResponseId(value) => Some(value),
        }
    }
}

impl fmt::Debug for ContinuationIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::PreviousResponseId(_) => formatter.write_str("PreviousResponseId(<redacted>)"),
        }
    }
}

/// Handler、Router 和 history owner 使用的请求元数据
#[derive(Clone, PartialEq, Eq)]
pub struct GenerationRequestMetadata {
    pub(in crate::openai) requested_model: String,
    pub(in crate::openai) stream: bool,
    pub(in crate::openai) store: bool,
    pub(in crate::openai) continuation: ContinuationIntent,
    pub(in crate::openai) client_ip: Option<IpAddr>,
    pub(in crate::openai) user_agent: Option<String>,
}

impl GenerationRequestMetadata {
    /// 返回客户端原始模型名
    #[must_use]
    pub fn requested_model(&self) -> &str {
        &self.requested_model
    }

    /// 返回客户端是否请求 SSE
    #[must_use]
    pub const fn stream(&self) -> bool {
        self.stream
    }

    /// 返回客户端 storage intent
    #[must_use]
    pub const fn store(&self) -> bool {
        self.store
    }

    /// 返回 continuation intent
    #[must_use]
    pub const fn continuation(&self) -> &ContinuationIntent {
        &self.continuation
    }

    /// 返回从 HTTP 连接边界解析出的客户端地址
    #[must_use]
    pub const fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    /// 返回经过 UTF-8 校验和空白归一化的 User-Agent
    #[must_use]
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }
}

impl fmt::Debug for GenerationRequestMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationRequestMetadata")
            .field("requested_model", &self.requested_model)
            .field("stream", &self.stream)
            .field("store", &self.store)
            .field("continuation", &self.continuation)
            .field("client_ip", &self.client_ip)
            .field("has_user_agent", &self.user_agent.is_some())
            .finish()
    }
}

/// 一次成功解码的生成或计数请求
#[derive(Clone, PartialEq)]
pub struct DecodedGenerationRequest {
    pub(in crate::openai) operation: Operation,
    pub(in crate::openai) metadata: GenerationRequestMetadata,
}

impl DecodedGenerationRequest {
    pub(crate) fn with_middleware_capabilities(
        mut self,
        request: &gateway_core::engine::middleware::MiddlewareRequest,
    ) -> Result<Self, gateway_core::engine::middleware::MiddlewareError> {
        self.operation = request.apply_capabilities(self.operation)?;
        Ok(self)
    }

    /// 附着当前 WebSocket 连接保存的 Provider 私有上一轮状态
    #[must_use]
    pub fn with_provider_session_state(mut self, state: ProviderSessionState) -> Self {
        self.operation = self.operation.with_provider_session_state(state);
        self
    }

    /// 返回协议无关 operation
    #[must_use]
    pub const fn operation(&self) -> &Operation {
        &self.operation
    }

    /// 返回 handler 元数据
    #[must_use]
    pub const fn metadata(&self) -> &GenerationRequestMetadata {
        &self.metadata
    }

    /// 拆分为 operation 与元数据
    #[must_use]
    pub fn into_parts(self) -> (Operation, GenerationRequestMetadata) {
        (self.operation, self.metadata)
    }

    /// 附着只由 HTTP/WebSocket 连接边界提供的诊断事实
    #[must_use]
    pub fn with_client_context(
        mut self,
        client_ip: Option<IpAddr>,
        user_agent: Option<String>,
    ) -> Self {
        self.metadata.client_ip = client_ip;
        self.metadata.user_agent = user_agent;
        self
    }
}

impl fmt::Debug for DecodedGenerationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedGenerationRequest")
            .field("operation", &self.operation)
            .field("metadata", &self.metadata)
            .finish()
    }
}
