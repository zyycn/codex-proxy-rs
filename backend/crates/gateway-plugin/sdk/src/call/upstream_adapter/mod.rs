//! 内置 OpenAI / xAI 账号上的上游转换合同，凭据不进入此线协议

use serde::{Deserialize, Serialize};

use super::model::ExecutionEvent;

mod codec;
pub use codec::encode_request_metadata;

pub const REGISTER_METHOD: &str = "upstream_adapter.register";
pub const EXECUTE_METHOD: &str = "upstream_adapter.execute";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuiltinProvider {
    OpenAi,
    Xai,
}

impl BuiltinProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Xai => "xai",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamTransport {
    HttpJson,
    HttpSse,
    #[serde(rename = "websocket")]
    WebSocket,
}

impl UpstreamTransport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpJson => "http_json",
            Self::HttpSse => "http_sse",
            Self::WebSocket => "websocket",
        }
    }
}

/// 注册结果随实例配置冻结；同一实例可为两个内置 Provider 分别声明适配器
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamAdapterRegistration {
    pub adapters: Vec<UpstreamAdapterDeclaration>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamAdapterDeclaration {
    pub id: String,
    pub provider: BuiltinProvider,
    pub base_url: String,
    /// 相对 Base URL 的精确业务路径，不接受绝对 URL、查询串或通配符
    pub paths: Vec<UpstreamPath>,
    pub authentication_kinds: Vec<String>,
    pub transport: UpstreamTransport,
    /// 客户端请求及适配完成后的 wire 协议；标准事实独立于 wire 传递
    pub protocol: String,
    /// 公开请求模型的匹配范围；空集合沿用功能绑定范围
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamPathPurpose {
    Inference,
    Auxiliary,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamPath {
    pub path: String,
    pub purpose: UpstreamPathPurpose,
}

/// 元数据为宿主只读投影；请求正文独立置于调用二进制载荷
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamAdapterRequest {
    pub adapter_id: String,
    pub provider: BuiltinProvider,
    pub upstream_model: String,
    pub client_key_id: String,
    pub account_id: String,
    pub credential_revision: u64,
    pub protocol: String,
    pub client_transport: String,
    /// 宿主冻结的 Fast 策略：default、enabled 或 disabled
    pub fast_mode: String,
    pub headers: Vec<(String, Vec<u8>)>,
    /// 只能来自同账号、同插件代次的宿主续接记录，不从客户端正文信任此值
    pub continuation: Option<UpstreamContinuation>,
}

impl UpstreamAdapterRequest {
    /// 请求身份、业务头与私有续接材料同原始正文一起放入二进制载荷
    pub fn encode(self, body: Vec<u8>) -> Result<Vec<u8>, super::model::ExecutionEncodingError> {
        codec::encode_request(self, body)
    }

    pub fn decode(bytes: &[u8]) -> Result<(Self, Vec<u8>), super::model::ExecutionEncodingError> {
        codec::decode_request(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationScope {
    Persisted,
    ConnectionLocal,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamContinuation {
    pub scope: ContinuationScope,
    pub upstream_response_id: String,
    /// 适配器私有的续接材料，不包含账号凭据
    pub state: serde_json::Map<String, serde_json::Value>,
}

/// 一个完整 GPE2 事件与可选的结算／续接事实；见 [`Self::encode`]
pub struct UpstreamAdapterEvent {
    pub event: ExecutionEvent,
    /// 上游回显的最新服务档位，可在终态确定；是否参与计价由内置 Provider 决定
    pub service_tier: Option<String>,
    pub continuation: Option<UpstreamContinuation>,
    pub failure: Option<UpstreamFailure>,
}

impl UpstreamAdapterEvent {
    #[must_use]
    pub const fn new(event: ExecutionEvent) -> Self {
        Self {
            event,
            service_tier: None,
            continuation: None,
            failure: None,
        }
    }

    /// 编码有界事件，wire 原字节仍使用 GPE2 二进制段
    pub fn encode(self) -> Result<Vec<u8>, super::model::ExecutionEncodingError> {
        codec::encode(self)
    }

    /// 校验版本、长度及元数据，再解码事件载荷
    pub fn decode(bytes: &[u8]) -> Result<Self, super::model::ExecutionEncodingError> {
        codec::decode(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamFailureKind {
    InvalidRequest,
    Unsupported,
    Unauthorized,
    PermissionDenied,
    RateLimited,
    QuotaExhausted,
    Timeout,
    Unavailable,
    Protocol,
}

/// 解析后的上游拒绝事实；发送状态始终由宿主网络观测，不接受插件声明
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamFailure {
    pub kind: UpstreamFailureKind,
    pub status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamHttpRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub query: Vec<(String, String)>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamWebSocketRequest {
    pub path: String,
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub query: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketMessageKind {
    Text,
    Binary,
}

/// 消息数据独立放入二进制载荷，宿主处理 Ping/Pong 与关闭握手
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamWebSocketMessage {
    pub kind: WebSocketMessageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamWebSocketRead {
    pub kind: Option<WebSocketMessageKind>,
    pub eof: bool,
}
