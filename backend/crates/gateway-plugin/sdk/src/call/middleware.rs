//! 统一请求/响应中间件的跨进程数据合同。

use serde::{Deserialize, Serialize};

pub const HANDLE_METHOD: &str = "middleware.handle";
pub const NEXT_METHOD: &str = "host.middleware.next";
pub const BODY_READ_METHOD: &str = "host.middleware.body_read";
pub const BODY_CLOSE_METHOD: &str = "host.middleware.body_close";

const BODY_FRAME_PREFIX: [u8; 4] = *b"GMB1";

/// 中间件在逻辑请求外层或单次 Provider attempt 内层执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareMount {
    Request,
    Attempt,
}

/// 客户端交付形态；它不授予插件访问底层 HTTP 或 WebSocket 连接。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareTransport {
    HttpJson,
    HttpSse,
    WebSocket,
    Internal,
}

/// 正文读取边界。结构化正文只按完整文档或完整 SSE 事件交付。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareBodyFraming {
    JsonDocument,
    SseEvent,
    RawBytes,
}

/// 一个保留顺序与重复项的 HTTP header。
///
/// Header 值可能包含不适合日志的客户端数据，因此故意不实现 `Debug`。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareHeader {
    pub name: String,
    pub value: Vec<u8>,
}

/// 对宿主保存的原始 header 集合做增量修改；未列出的项保持原样。
///
/// Header 值可能包含不适合日志的客户端数据，因此故意不实现 `Debug`。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MiddlewareHeaderMutation {
    Remove { name: String },
    Append { name: String, value: Vec<u8> },
}

/// `middleware.handle` 的请求头。正文位于 RPC binary payload；未授权读取时为空。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareRequestHead {
    pub request_id: String,
    pub mount: MiddlewareMount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_index: Option<u32>,
    pub operation: String,
    pub protocol: String,
    pub endpoint: String,
    pub transport: MiddlewareTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default)]
    pub headers: Vec<MiddlewareHeader>,
    /// `true` 表示 binary payload 是完整请求正文；`false` 表示正文未向插件投影。
    pub body_visible: bool,
}

/// 调用 next 时如何处理宿主保存的原始请求正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareRequestBody {
    Preserve,
    Replace,
}

/// 可声明的请求功能；原生续接始终由宿主与 Provider 管理，不能由声明豁免。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestFeature {
    Tools,
    Vision,
    Reasoning,
    JsonSchema,
}

/// middleware v2：插件承担的功能转换与额外上游需求，均不覆盖正文推导的事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDeclaration {
    pub handled: Vec<RequestFeature>,
    pub required: Vec<RequestFeature>,
}

/// 单次 `host.middleware.next` 调用。`Replace` 的完整正文位于 binary payload。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareNextRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default)]
    pub header_mutations: Vec<MiddlewareHeaderMutation>,
    pub body: MiddlewareRequestBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<CapabilityDeclaration>,
}

/// 受父调用约束的不透明响应正文句柄。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareBodyHandle {
    pub handle: String,
    pub framing: MiddlewareBodyFraming,
}

/// `host.middleware.next` 返回的响应头。正文必须通过句柄惰性读取或原样返还。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareNextResponse {
    /// 绑定本次 next 结果的宿主响应身份；只能在当前父调用内返还。
    pub response: String,
    pub protocol: String,
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<MiddlewareHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<MiddlewareBodyHandle>,
}

/// 最终正文来源。`PassThrough` 不读取正文，也不要求响应正文读写授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MiddlewareResponseBody {
    Empty,
    PassThrough { body: MiddlewareBodyHandle },
    Stream { framing: MiddlewareBodyFraming },
}

/// `middleware.handle` 的最终响应头；插件输出流仍使用既有 RPC Stream/Credit/End。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareResponseHead {
    /// `Some` 表示基于本层 next 返回的原响应；`None` 表示中间件短路响应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    /// 缺省时保留原响应协议；短路响应必须填写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// 缺省时保留原状态；短路响应必须填写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default)]
    pub header_mutations: Vec<MiddlewareHeaderMutation>,
    pub body: MiddlewareResponseBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareBodyRead {
    pub handle: String,
    pub maximum_bytes: u32,
}

/// `eof` 只在空 payload 时成立；结构化 frame 不拆成任意网络分块。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareBodyReadResult {
    pub framing: MiddlewareBodyFraming,
    /// Runtime 为每个输入 frame 分配的非零单调标识；EOF 时必须为零。
    pub source_id: u64,
    pub eof: bool,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareBodyClose {
    pub handle: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareBodyCloseResult {}

/// 一个输出 frame 与其来源的 wire disposition。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MiddlewareBodyDisposition {
    Standalone = 0,
    Only = 1,
    First = 2,
    More = 3,
    Last = 4,
    Drop = 5,
}

/// 插件替换正文流中的一个完整 frame；正文不进入 JSON metadata。
#[derive(Clone, PartialEq, Eq)]
pub struct MiddlewareBodyFrame {
    pub payload: Vec<u8>,
    pub terminal: bool,
    source_id: u64,
    disposition: MiddlewareBodyDisposition,
}

impl MiddlewareBodyFrame {
    #[must_use]
    pub fn new(payload: Vec<u8>, terminal: bool) -> Self {
        Self {
            payload,
            terminal,
            source_id: 0,
            disposition: MiddlewareBodyDisposition::Standalone,
        }
    }

    #[cfg(feature = "io")]
    pub(crate) fn from_source(
        payload: Vec<u8>,
        terminal: bool,
        source_id: u64,
    ) -> Result<Self, MiddlewareBodyFrameError> {
        if source_id == 0 {
            return Err(MiddlewareBodyFrameError);
        }
        Ok(Self {
            payload,
            terminal,
            source_id,
            disposition: MiddlewareBodyDisposition::Only,
        })
    }

    #[cfg(feature = "io")]
    pub(crate) fn map_to_source(
        mut self,
        source_id: u64,
        disposition: MiddlewareBodyDisposition,
    ) -> Result<Self, MiddlewareBodyFrameError> {
        if source_id == 0
            || matches!(disposition, MiddlewareBodyDisposition::Standalone)
            || (matches!(disposition, MiddlewareBodyDisposition::Drop) && !self.payload.is_empty())
        {
            return Err(MiddlewareBodyFrameError);
        }
        self.source_id = source_id;
        self.disposition = disposition;
        // Mapped frame 的终态只由 Runtime 保存的源 frame 派生。
        self.terminal = false;
        Ok(self)
    }

    #[must_use]
    pub const fn source_id(&self) -> u64 {
        self.source_id
    }

    #[must_use]
    pub const fn disposition(&self) -> MiddlewareBodyDisposition {
        self.disposition
    }

    /// 将 frame 元数据与原始正文编码到一个 RPC stream chunk。
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut encoded = Vec::with_capacity(14 + self.payload.len());
        encoded.extend_from_slice(&BODY_FRAME_PREFIX);
        encoded.push(self.disposition as u8);
        encoded.extend_from_slice(&self.source_id.to_be_bytes());
        encoded.push(u8::from(self.terminal));
        encoded.extend_from_slice(&self.payload);
        encoded
    }

    /// 解码一个完整的中间件正文 frame。
    ///
    /// # Errors
    ///
    /// 魔数或 flags 无效时返回错误。
    pub fn decode(bytes: &[u8]) -> Result<Self, MiddlewareBodyFrameError> {
        if bytes.len() < 14 || bytes[..4] != BODY_FRAME_PREFIX || bytes[13] > 1 {
            return Err(MiddlewareBodyFrameError);
        }
        let disposition = match bytes[4] {
            0 => MiddlewareBodyDisposition::Standalone,
            1 => MiddlewareBodyDisposition::Only,
            2 => MiddlewareBodyDisposition::First,
            3 => MiddlewareBodyDisposition::More,
            4 => MiddlewareBodyDisposition::Last,
            5 => MiddlewareBodyDisposition::Drop,
            _ => return Err(MiddlewareBodyFrameError),
        };
        let source_id = u64::from_be_bytes(
            bytes[5..13]
                .try_into()
                .map_err(|_| MiddlewareBodyFrameError)?,
        );
        let terminal = bytes[13] == 1;
        if (source_id == 0) != matches!(disposition, MiddlewareBodyDisposition::Standalone)
            || (source_id != 0 && terminal)
            || (matches!(disposition, MiddlewareBodyDisposition::Drop) && bytes.len() != 14)
        {
            return Err(MiddlewareBodyFrameError);
        }
        Ok(Self {
            payload: bytes[14..].to_vec(),
            terminal,
            source_id,
            disposition,
        })
    }
}

impl std::fmt::Debug for MiddlewareBodyFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MiddlewareBodyFrame")
            .field("payload_bytes", &self.payload.len())
            .field("terminal", &self.terminal)
            .field("source_id", &self.source_id)
            .field("disposition", &self.disposition)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("middleware body frame is invalid")]
pub struct MiddlewareBodyFrameError;
