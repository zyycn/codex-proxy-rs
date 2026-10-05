//! Codex Live / Realtime 语音会话的 Core 合同。
//!
//! 语音会话由两类请求组成：一次性 SDP 引导（走常规引擎 attempt 路径）和
//! 通话建立后跟随 call id 的长连接 sideband 中继。sideband 不能进入
//! 请求/响应引擎，Provider 通过本模块的 [`LiveGateway`] 暴露受限的
//! "钉住账号拨号 + 帧中继" 能力，协议 adapter 只消费抽象帧。

use std::fmt;

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::account::scope::FrozenAccountScope;
use crate::policy::ClientApiKeyId;

/// sideband 中继在客户端与上游 WebSocket 之间搬运的帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveFrame {
    Text(Bytes),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close(Option<LiveClose>),
}

/// WebSocket 关闭帧的状态码与原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveClose {
    pub code: u16,
    pub reason: String,
}

/// sideband 的上游目标形态；决定钉住账号拨号使用的路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSidebandStyle {
    /// `GET /v1/live/{call_id}` → 上游 `wss://…/v1/live/{call_id}`。
    Live,
    /// `GET /v1/realtime/calls/{call_id}` → 上游 `wss://…/v1/realtime/calls/{call_id}`。
    RealtimeCalls,
    /// `GET /v1/realtime?call_id=…` → 上游 `wss://…/v1/realtime?intent=quicksilver&call_id=…`。
    RealtimeQuery,
}

/// sideband 拨号请求；协议头已由协议 adapter 按允许清单过滤。
#[derive(Debug)]
pub struct LiveSidebandRequest<'a> {
    pub call_id: &'a str,
    pub client_api_key_id: &'a ClientApiKeyId,
    /// 本次认证冻结的账号范围与模型政策，重连也必须重新校验
    pub account_scope: &'a FrozenAccountScope,
    pub style: LiveSidebandStyle,
    pub protocol_headers: Vec<(String, String)>,
    /// 客户端 offer 的 `Sec-WebSocket-Protocol`，原样透传给上游。
    pub subprotocols: Vec<String>,
}

/// 一次性账号级上游调用（hangup）请求。
#[derive(Debug)]
pub struct LiveHangupRequest<'a> {
    pub call_id: &'a str,
    pub client_api_key_id: &'a ClientApiKeyId,
    pub content_type: Option<String>,
    pub body: Bytes,
    pub protocol_headers: Vec<(String, String)>,
}

/// 一次性账号级上游调用的透传结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveCallOutcome {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// sideband 中继的上游半边；帧按原始顺序搬运，不做协议解释。
///
/// 中继可以携带一个会话 guard（如 call 的账号认领）；`LiveRelay` 被
/// 丢弃时 guard 一并释放，覆盖传输中断、升级失败与任务取消等全部退出路径。
pub struct LiveRelay {
    /// 上游已协商的 `Sec-WebSocket-Protocol`；协议 adapter 据此回写下游。
    pub subprotocol: Option<String>,
    receiver: Box<dyn LiveRelayStream>,
    sender: Box<dyn LiveRelaySink>,
    guard: Option<Box<dyn LiveRelayGuard>>,
}

/// 随 [`LiveRelay`] 存活的会话占用标记；丢弃时由实现方归还资源。
pub trait LiveRelayGuard: Send {}

impl LiveRelay {
    /// 由 Provider 实现装配；Core 不感知底层 WebSocket 栈。
    #[must_use]
    pub fn new(
        subprotocol: Option<String>,
        receiver: Box<dyn LiveRelayStream>,
        sender: Box<dyn LiveRelaySink>,
    ) -> Self {
        Self {
            subprotocol,
            receiver,
            sender,
            guard: None,
        }
    }

    /// 附加随中继释放的会话 guard；重复调用以最后一次为准。
    pub fn with_guard(&mut self, guard: Box<dyn LiveRelayGuard>) {
        self.guard = Some(guard);
    }

    /// 读取上游下一帧；返回 `None` 表示上游已关闭。
    pub fn next_frame(&mut self) -> BoxFuture<'_, Option<LiveFrame>> {
        self.receiver.next_frame()
    }

    /// 向上游写入一帧。
    pub fn send_frame(&mut self, frame: LiveFrame) -> BoxFuture<'_, Result<(), LiveGatewayError>> {
        self.sender.send_frame(frame)
    }

    /// 主动关闭上游半边。
    pub fn close(&mut self, close: Option<LiveClose>) -> BoxFuture<'_, ()> {
        self.sender.close(close)
    }
}

impl fmt::Debug for LiveRelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveRelay")
            .field("subprotocol", &self.subprotocol)
            .finish()
    }
}

/// 上游侧的帧接收端。
pub trait LiveRelayStream: Send {
    fn next_frame(&mut self) -> BoxFuture<'_, Option<LiveFrame>>;
}

/// 上游侧的帧发送端。
pub trait LiveRelaySink: Send {
    fn send_frame(&mut self, frame: LiveFrame) -> BoxFuture<'_, Result<(), LiveGatewayError>>;
    fn close(&mut self, close: Option<LiveClose>) -> BoxFuture<'_, ()>;
}

/// Live 会话在 Provider 内的故障分类；协议 adapter 据此投影 HTTP 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveGatewayErrorKind {
    /// call id 不存在或已过期。
    CallNotFound,
    /// 该 call 已有 sideband 加入。
    CallBusy,
    /// 调用方不是创建该 call 的 API Key，或固定账号不在当前授权范围
    OwnerMismatch,
    /// call id 不满足上游标识约束。
    InvalidCallId,
    /// 钉住的账号凭据不可用。
    CredentialUnavailable,
    /// 上游握手或调用失败。
    UpstreamUnavailable,
    /// 该 Provider 不支持请求的 live 能力。
    Unsupported,
}

impl LiveGatewayErrorKind {
    /// 错误分类的默认 HTTP 状态；上游显式状态优先于该映射。
    #[must_use]
    pub const fn default_status(self) -> u16 {
        match self {
            Self::CallNotFound => 404,
            Self::CallBusy => 409,
            Self::OwnerMismatch | Self::InvalidCallId => 403,
            Self::CredentialUnavailable | Self::UpstreamUnavailable => 503,
            Self::Unsupported => 501,
        }
    }

    /// 面向 OpenAI realtime 客户端的稳定错误码。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::CallNotFound => "realtime_call_not_found",
            Self::CallBusy => "realtime_call_busy",
            Self::OwnerMismatch => "realtime_call_scope_mismatch",
            Self::InvalidCallId => "invalid_call_id",
            Self::CredentialUnavailable => "codex_auth_unavailable",
            Self::UpstreamUnavailable => "realtime_upstream_unavailable",
            Self::Unsupported => "realtime_capability_not_supported",
        }
    }
}

/// Live 会话操作失败；`status`/`body` 携带上游显式响应时优先透传。
#[derive(Debug, Clone)]
pub struct LiveGatewayError {
    kind: LiveGatewayErrorKind,
    message: String,
    status: Option<u16>,
    body: Option<Bytes>,
}

impl LiveGatewayError {
    #[must_use]
    pub fn new(kind: LiveGatewayErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
            body: None,
        }
    }

    #[must_use]
    pub fn with_upstream(mut self, status: u16, body: Option<Bytes>) -> Self {
        self.status = Some(status);
        self.body = body;
        self
    }

    #[must_use]
    pub const fn kind(&self) -> LiveGatewayErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 投影给客户端的 HTTP 状态；上游显式状态优先。
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self.status {
            Some(status) => status,
            None => self.kind.default_status(),
        }
    }

    #[must_use]
    pub fn body(&self) -> Option<&Bytes> {
        self.body.as_ref()
    }
}

impl fmt::Display for LiveGatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for LiveGatewayError {}

/// Provider 暴露给协议 adapter 的 Live sideband 能力。
///
/// 实现方负责把 call id 钉到创建账号、复验调用方身份，并用该账号的
/// 运行时凭据拨号；Core 与协议 adapter 不接触账号凭据。
///
/// call 绑定的生命周期由实现方管理：sideband 断开只释放占用，绑定保留到
/// 会话 TTL（官方 FramelessBidi 客户端会在传输中断后重连同一 call）；
/// 确认结束（挂断成功、上游报告通话不存在）才删除绑定。
pub trait LiveGateway: Send + Sync {
    /// 为已建立的 call 建立账号级上游 sideband 中继。
    ///
    /// 返回的 [`LiveRelay`] 携带本次占用的 guard；中继结束即释放占用，
    /// 协议 adapter 无需另行通知会话结束。
    fn open_sideband<'a>(
        &'a self,
        request: LiveSidebandRequest<'a>,
    ) -> BoxFuture<'a, Result<LiveRelay, LiveGatewayError>>;

    /// 以钉住账号转发一次性调用（当前仅 hangup）。
    fn hangup<'a>(
        &'a self,
        request: LiveHangupRequest<'a>,
    ) -> BoxFuture<'a, Result<LiveCallOutcome, LiveGatewayError>>;
}

/// call id 形态校验；与上游 `Location` 的可解析范围一致。
#[must_use]
pub fn is_valid_call_id(call_id: &str) -> bool {
    !call_id.is_empty()
        && call_id.len() <= 128
        && call_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// 从上游 `Location` 提取 call id；接受裸 id、路径尾段与 `?call_id=` 三种形态。
///
/// Location 的形态由上游合同决定（`…/live/{id}`、`…/calls/{id}`、
/// `…/realtime?call_id={id}`、裸 id），不引入完整 URL 解析依赖。
#[must_use]
pub fn call_id_from_location(location: &str) -> Option<String> {
    let location = location.trim();
    if is_valid_call_id(location) {
        return Some(location.to_owned());
    }
    if let Some(start) = location.find("call_id=") {
        let rest = &location[start + "call_id=".len()..];
        let candidate = rest.split('&').next().unwrap_or(rest);
        if is_valid_call_id(candidate) {
            return Some(candidate.to_owned());
        }
    }
    let after_authority = location.split_once("//").map(|(_, rest)| rest);
    let path = match after_authority {
        Some(rest) => rest.split_once('/').map(|(_, path)| path).unwrap_or(""),
        None => location,
    };
    let segments = path
        .split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return None;
    }
    let call_id = segments[segments.len() - 1];
    let previous = segments[segments.len() - 2];
    if is_valid_call_id(call_id) && (previous == "live" || previous == "calls") {
        return Some(call_id.to_owned());
    }
    None
}
