//! 原生 Provider 的执行边界。

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt, future::BoxFuture};
use thiserror::Error;

use crate::account::{
    AccountAttemptFeedback, AccountCapacitySnapshot, AccountFeedbackStats, ProviderAccountId,
};
use crate::engine::AttemptContext;
use crate::engine::execution::ClientTransport;
use crate::engine::middleware::{
    MiddlewareBody, MiddlewareContext, MiddlewareError, MiddlewareFrame, MiddlewareFraming,
    MiddlewareHeader, MiddlewareNext, MiddlewareRequest, MiddlewareResponse,
};
use crate::error::{PreDeliveryRetry, ProviderError, ProviderErrorKind};
use crate::event::{EventSequenceValidator, GatewayEvent, ProtocolWireEvent, ProviderEvent};
use crate::identity::ProviderKind;
use crate::operation::Operation;
use crate::policy::ClientApiKeyId;
use crate::routing::{
    ProviderCandidate, ProviderCatalogGeneration, ProviderCatalogPort, ProviderCatalogUnavailable,
    ProviderModelCapabilities, PublicModelId, UpstreamModelId,
};
use crate::upstream::OpaqueUpstreamValue;
use crate::upstream::{UpstreamSendState, UpstreamTransport};

/// Box 只出现在 Provider Registry 的统一 event envelope 边界。
pub type EventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send + 'static>>;

/// Provider 选定单个 credential 后返回的事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCallMetadata {
    provider: ProviderKind,
    upstream_model: Option<UpstreamModelId>,
    provider_account_id: ProviderAccountId,
    upstream_request_id: Option<OpaqueUpstreamValue>,
    transport: UpstreamTransport,
    selection_observation: Option<ProviderSelectionObservation>,
}

/// Provider 账号选择阶段输出的中立运行压力事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSelectionObservation {
    account_selection_wait_ms: u64,
    capacity: Option<AccountCapacitySnapshot>,
}

impl ProviderSelectionObservation {
    #[must_use]
    pub const fn new(
        account_selection_wait_ms: u64,
        capacity: Option<AccountCapacitySnapshot>,
    ) -> Self {
        Self {
            account_selection_wait_ms,
            capacity,
        }
    }

    #[must_use]
    pub const fn account_selection_wait_ms(self) -> u64 {
        self.account_selection_wait_ms
    }

    #[must_use]
    pub const fn capacity(self) -> Option<AccountCapacitySnapshot> {
        self.capacity
    }
}

impl ProviderCallMetadata {
    /// 创建一次调用的不可变事实。
    #[must_use]
    pub const fn new(
        provider: ProviderKind,
        upstream_model: UpstreamModelId,
        provider_account_id: ProviderAccountId,
        transport: UpstreamTransport,
    ) -> Self {
        Self {
            provider,
            upstream_model: Some(upstream_model),
            provider_account_id,
            upstream_request_id: None,
            transport,
            selection_observation: None,
        }
    }

    /// 创建一次不声明模型的 Provider 原生端点调用事实。
    #[must_use]
    pub const fn for_provider_endpoint(
        provider: ProviderKind,
        provider_account_id: ProviderAccountId,
        transport: UpstreamTransport,
    ) -> Self {
        Self {
            provider,
            upstream_model: None,
            provider_account_id,
            upstream_request_id: None,
            transport,
            selection_observation: None,
        }
    }

    /// 设置 adapter 已分类为非 bearer 的 request ID。
    #[must_use]
    pub fn with_upstream_request_id(mut self, request_id: OpaqueUpstreamValue) -> Self {
        self.upstream_request_id = Some(request_id);
        self
    }

    /// 附加 Provider 选择账号时测得的等待与容量快照。
    #[must_use]
    pub const fn with_selection_observation(
        mut self,
        observation: ProviderSelectionObservation,
    ) -> Self {
        self.selection_observation = Some(observation);
        self
    }

    /// 返回 Provider。
    #[must_use]
    pub const fn provider(&self) -> &ProviderKind {
        &self.provider
    }

    /// 返回实际模型；Provider 原生端点没有模型事实。
    #[must_use]
    pub const fn upstream_model(&self) -> Option<&UpstreamModelId> {
        self.upstream_model.as_ref()
    }

    /// 返回 live Provider account ID。
    #[must_use]
    pub const fn provider_account_id(&self) -> &ProviderAccountId {
        &self.provider_account_id
    }

    /// 返回安全上游 request ID。
    #[must_use]
    pub const fn upstream_request_id(&self) -> Option<&OpaqueUpstreamValue> {
        self.upstream_request_id.as_ref()
    }

    /// 返回 transport。
    #[must_use]
    pub const fn transport(&self) -> &UpstreamTransport {
        &self.transport
    }

    #[must_use]
    pub const fn selection_observation(&self) -> Option<ProviderSelectionObservation> {
        self.selection_observation
    }

    /// 确认 metadata 没有替换请求计划中冻结的 Provider 候选。
    #[must_use]
    pub fn confirms(&self, candidate: &ProviderCandidate) -> bool {
        candidate.provider() == &self.provider
            && candidate.upstream_model() == self.upstream_model.as_ref()
    }
}

/// Provider 自己持有的 credential/concurrency 租约。
///
/// 句柄必须通过 `Drop` 释放资源。Core 不读取 credential，也不为 lease 定义
/// 平台无关字段。
pub trait ResourceLease: Send + Sync + 'static {}

impl<T> ResourceLease for T where T: Send + Sync + 'static {}

/// Provider 原生响应格式到统一响应格式的有状态转换边界。
///
/// Core 只负责调用顺序、格式与终态约束，不解释协议正文。实现必须由一次
/// [`ProviderStream`] 独占，不能在并发请求间共享可变转换状态。
pub trait NativeResponseTranslator: Send + 'static {
    /// 转换前协议格式。
    fn source_protocol(&self) -> &str;

    /// 转换后协议格式。
    fn target_protocol(&self) -> &str;

    /// 把一个原生响应事件转换为零至多个统一响应事件。
    ///
    /// # Errors
    ///
    /// 原生协议事件无效或转换状态不一致时返回已发送的 Provider 错误。
    fn translate(
        &mut self,
        event: &crate::event::ProtocolWireEvent,
    ) -> Result<Vec<crate::event::ProtocolWireEvent>, ProviderError>;
}

const MAX_NATIVE_RESPONSE_EVENTS_PER_INPUT: usize = 64;

/// Metadata、canonical event stream 与 owned lease 的统一返回值。
///
/// 底层 stream 必须是 cold stream：在第一次 poll 前不得发送请求级 handshake
/// 或业务 payload。这样 Coordinator 可以先持久化 attempt，再越过发送屏障。
pub struct ProviderStream {
    metadata: ProviderCallMetadata,
    events: EventStream,
    _lease: Box<dyn ResourceLease>,
    native_response_translator: Option<Box<dyn NativeResponseTranslator>>,
    account_feedback: Option<ProviderStreamAccountFeedback>,
    validator: EventSequenceValidator,
    strict_canonical_seen: bool,
    terminated: bool,
}

struct ProviderStreamAccountFeedback {
    stats: Arc<AccountFeedbackStats>,
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    failure_filter: fn(&ProviderError) -> bool,
    started_at: Option<Instant>,
    first_output_ms: Option<u64>,
    reported: bool,
}

fn score_all_confirmed_failures(_: &ProviderError) -> bool {
    true
}

impl ProviderStreamAccountFeedback {
    fn start(&mut self) {
        self.started_at.get_or_insert_with(Instant::now);
    }

    fn observe(&mut self, event: &ProviderEvent) {
        if self.first_output_ms.is_some()
            || !event.canonical_facts().iter().any(|event| {
                matches!(
                    event,
                    crate::event::GatewayEvent::TextDelta(_)
                        | crate::event::GatewayEvent::ReasoningDelta(_)
                        | crate::event::GatewayEvent::ToolCallDelta(_)
                )
            })
        {
            return;
        }
        let Some(started_at) = self.started_at else {
            return;
        };
        self.first_output_ms =
            Some(u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX));
    }

    fn report_success(&mut self) {
        if self.reported {
            return;
        }
        self.stats.report(
            &self.provider_kind,
            &self.account_id,
            AccountAttemptFeedback::Succeeded {
                first_output_ms: self.first_output_ms,
            },
        );
        self.reported = true;
    }

    fn report_failure(&mut self, error: &ProviderError) {
        if self.reported
            // `Ambiguous` 仍须关闭重放边界，但不能证明失败由账号造成；将它计入
            // 账号评分会把传输不确定性错误归因给账号。
            || error.send_state() != UpstreamSendState::Sent
            || matches!(
                error.kind(),
                ProviderErrorKind::Cancelled | ProviderErrorKind::ProcessTerminated
            )
            || matches!(
                error.pre_delivery_retry(),
                Some(
                    PreDeliveryRetry::SameAccountTransportRetry { .. }
                        | PreDeliveryRetry::SameAccountTransportFallback
                )
            )
            || error.retries_same_account()
            || !(self.failure_filter)(error)
        {
            return;
        }
        let feedback = if error.kind() == ProviderErrorKind::UpstreamCapacityUnavailable {
            AccountAttemptFeedback::CapacityRejected {
                first_output_ms: self.first_output_ms,
            }
        } else {
            AccountAttemptFeedback::Failed {
                first_output_ms: self.first_output_ms,
            }
        };
        self.stats
            .report(&self.provider_kind, &self.account_id, feedback);
        self.reported = true;
    }
}

impl ProviderStream {
    /// 组装一次、且仅一次可见上游调用。
    #[must_use]
    pub fn new<S>(metadata: ProviderCallMetadata, events: S, lease: impl ResourceLease) -> Self
    where
        S: Stream<Item = Result<ProviderEvent, ProviderError>> + Send + 'static,
    {
        Self {
            metadata,
            events: Box::pin(events),
            _lease: Box::new(lease),
            native_response_translator: None,
            account_feedback: None,
            validator: EventSequenceValidator::new(),
            strict_canonical_seen: false,
            terminated: false,
        }
    }

    /// 让公共 stream 边界统一回灌账号成功率与首个有效输出延迟。
    #[must_use]
    pub fn with_account_feedback(mut self, stats: Arc<AccountFeedbackStats>) -> Self {
        self.set_account_feedback(stats, score_all_confirmed_failures);
        self
    }

    /// 使用 Provider 定义的闭集判断回灌账号成功率与首个有效输出延迟。
    #[must_use]
    pub fn with_filtered_account_feedback(
        mut self,
        stats: Arc<AccountFeedbackStats>,
        failure_filter: fn(&ProviderError) -> bool,
    ) -> Self {
        self.set_account_feedback(stats, failure_filter);
        self
    }

    /// 在原生响应加工的两个策略阶段之间安装 Provider 自有转换器。
    #[must_use]
    pub fn with_native_response_translator(
        mut self,
        translator: impl NativeResponseTranslator,
    ) -> Self {
        self.native_response_translator = Some(Box::new(translator));
        self
    }

    /// 返回本次 stream 是否需要原生响应转换。
    #[must_use]
    pub const fn has_native_response_translator(&self) -> bool {
        self.native_response_translator.is_some()
    }

    /// 在 Core 已记录原始事实且完成 `BeforeTranslation` 后执行原生转换。
    ///
    /// # Errors
    ///
    /// source/target 格式不一致、单事件展开越界、终态被丢弃或 Provider 转换失败时
    /// 返回错误。
    pub fn translate_native_response(
        &mut self,
        mut event: ProviderEvent,
        terminal: bool,
    ) -> Result<Vec<ProviderEvent>, ProviderError> {
        let Some(translator) = self.native_response_translator.as_mut() else {
            return Ok(vec![event]);
        };
        let middleware_transformed = event.middleware_transformed();
        let Some(wire) = event.wire_event() else {
            return Ok(vec![event]);
        };
        if wire.protocol() != translator.source_protocol() {
            return Err(native_response_protocol_error());
        }
        let mut translated = translator.translate(wire)?;
        if translated.len() > MAX_NATIVE_RESPONSE_EVENTS_PER_INPUT
            || (terminal && translated.is_empty())
            || translated
                .iter()
                .any(|wire| wire.protocol() != translator.target_protocol())
        {
            return Err(native_response_protocol_error());
        }
        let Some(last) = translated.pop() else {
            // wire 转换器可以吞掉纯协议结构事件，但不能连带丢失 Core 已经
            // 识别出的 usage 等 canonical facts。facts-only 封套仍需交给
            // 客户端 adapter；没有 facts 时才是真正的零输出。
            event.replace_wire(None);
            return Ok(event
                .has_canonical_facts()
                .then_some(event)
                .into_iter()
                .collect());
        };
        // 一对多时把 canonical/observation/session 信封放到最后一个 wire；这样
        // `Completed` 不会在同一源事件的前置输出交付前提前终结 Coordinator。
        let mut events = translated
            .into_iter()
            .map(|wire| {
                let mut translated_event = ProviderEvent::wire(wire);
                if middleware_transformed {
                    translated_event.inherit_middleware_provenance(&event);
                }
                translated_event
            })
            .collect::<Vec<_>>();
        event.replace_wire(Some(last));
        events.push(event);
        Ok(events)
    }

    fn set_account_feedback(
        &mut self,
        stats: Arc<AccountFeedbackStats>,
        failure_filter: fn(&ProviderError) -> bool,
    ) {
        self.account_feedback = Some(ProviderStreamAccountFeedback {
            stats,
            provider_kind: self.metadata.provider().clone(),
            account_id: self.metadata.provider_account_id().clone(),
            failure_filter,
            started_at: None,
            first_output_ms: None,
            reported: false,
        });
    }

    /// 返回调用事实。
    #[must_use]
    pub const fn metadata(&self) -> &ProviderCallMetadata {
        &self.metadata
    }

    fn middleware_protocol(&self, fallback: &str) -> String {
        self.native_response_translator.as_ref().map_or_else(
            || fallback.to_owned(),
            |translator| translator.target_protocol().to_owned(),
        )
    }
}

/// Attempt 中间件消费一次后建立 cold Provider stream 的 owned terminal。
pub type ProviderMiddlewareTerminal = Box<
    dyn FnOnce(
            Operation,
            Vec<MiddlewareHeader>,
        ) -> BoxFuture<'static, Result<ProviderStream, ProviderError>>
        + Send
        + 'static,
>;

const MAX_ATTEMPT_HEADERS: usize = 128;
const MAX_ATTEMPT_HEADER_NAME_BYTES: usize = 128;
const MAX_ATTEMPT_HEADER_VALUE_BYTES: usize = 16 * 1024;
const MAX_ATTEMPT_HEADER_TOTAL_BYTES: usize = 64 * 1024;

struct ProviderMiddlewareNext {
    operation: Operation,
    transport: ClientTransport,
    terminal: Option<ProviderMiddlewareTerminal>,
}

impl MiddlewareNext for ProviderMiddlewareNext {
    fn run(
        mut self: Box<Self>,
        request: MiddlewareRequest,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async move {
            let fallback_protocol = self.operation.protocol().to_owned();
            if request.has_capability_declaration() {
                return Err(MiddlewareError::InvalidState);
            }
            let (protocol, headers, body) = request.into_parts();
            validate_attempt_headers(&headers)?;
            let operation = self
                .operation
                .replace_middleware_wire(protocol, body)
                .map_err(|_| MiddlewareError::InvalidState)?;
            let terminal = self.terminal.take().ok_or(MiddlewareError::InvalidState)?;
            let stream = terminal(operation, headers)
                .await
                .map_err(MiddlewareError::Provider)?;
            let metadata = stream.metadata().clone();
            let protocol = stream.middleware_protocol(&fallback_protocol);
            let body = ProviderMiddlewareBody {
                stream,
                pending: VecDeque::new(),
                transport: self.transport,
            };
            Ok(
                MiddlewareResponse::new(protocol, 200, Vec::new(), Box::new(body))
                    .with_provider_metadata(metadata),
            )
        })
    }
}

fn validate_attempt_headers(headers: &[MiddlewareHeader]) -> Result<(), MiddlewareError> {
    if headers.len() > MAX_ATTEMPT_HEADERS {
        return Err(MiddlewareError::InvalidState);
    }
    let mut total = 0_usize;
    for header in headers {
        let name = header.name();
        let normalized = name.to_ascii_lowercase();
        if name.is_empty()
            || name.len() > MAX_ATTEMPT_HEADER_NAME_BYTES
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || header.value().len() > MAX_ATTEMPT_HEADER_VALUE_BYTES
            || header
                .value()
                .iter()
                .any(|byte| *byte != b'\t' && (*byte < b' ' || *byte == 0x7f))
            || attempt_header_is_protected(&normalized)
        {
            return Err(MiddlewareError::InvalidState);
        }
        total = total
            .checked_add(name.len())
            .and_then(|value| value.checked_add(header.value().len()))
            .ok_or(MiddlewareError::InvalidState)?;
        if total > MAX_ATTEMPT_HEADER_TOTAL_BYTES {
            return Err(MiddlewareError::InvalidState);
        }
    }
    Ok(())
}

fn attempt_header_is_protected(name: &str) -> bool {
    name.contains("auth")
        || name.contains("credential")
        || name.contains("secret")
        || name.contains("token")
        || name.contains("cookie")
        || name.contains("session")
        || name.contains("conversation")
        || name.contains("thread")
        || name.contains("account")
        || name.contains("organization")
        || name.contains("project")
        || name.contains("tenant")
        || name.contains("principal")
        || name.contains("identity")
        || name.contains("user-id")
        || name.ends_with("-key")
        || name.ends_with("_key")
        || name.starts_with("sec-websocket-")
        || matches!(
            name,
            "connection"
                | "keep-alive"
                | "proxy-connection"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "host"
                | "content-length"
                | "content-type"
                | "content-encoding"
                | "accept"
                | "accept-encoding"
                | "user-agent"
                | "x-request-id"
                | "x-gateway-request-id"
        )
}

struct ProviderMiddlewareBody {
    stream: ProviderStream,
    pending: VecDeque<(ProviderEvent, bool)>,
    transport: ClientTransport,
}

impl MiddlewareBody for ProviderMiddlewareBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            loop {
                if let Some((event, terminal)) = self.pending.pop_front() {
                    return provider_event_to_middleware_frame(event, self.transport, terminal)
                        .map(Some)
                        .map_err(MiddlewareError::Provider);
                }
                let Some(event) = self.stream.next().await else {
                    return Ok(None);
                };
                let event = event.map_err(MiddlewareError::Provider)?;
                let terminal = event
                    .canonical_facts()
                    .iter()
                    .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
                let translated = self
                    .stream
                    .translate_native_response(event, terminal)
                    .map_err(MiddlewareError::Provider)?;
                let last = translated.len().saturating_sub(1);
                self.pending.extend(
                    translated
                        .into_iter()
                        .enumerate()
                        .map(|(index, event)| (event, terminal && index == last)),
                );
            }
        })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { drop(self) })
    }
}

fn provider_event_to_middleware_frame(
    event: ProviderEvent,
    transport: ClientTransport,
    terminal: bool,
) -> Result<MiddlewareFrame, ProviderError> {
    let Some(wire) = event.wire_event() else {
        return Ok(MiddlewareFrame::from_provider_event(
            Bytes::new(),
            MiddlewareFraming::RawBytes,
            terminal,
            event,
        ));
    };
    let (bytes, framing) = match transport {
        ClientTransport::HttpSse => (
            wire.raw_sse_frame()
                .cloned()
                .map_or_else(|| encode_sse_wire_event(wire), Ok)?,
            MiddlewareFraming::SseEvent,
        ),
        ClientTransport::HttpJson => (
            wire.raw_json_body()
                .or_else(|| wire.raw_http_body_bytes())
                .cloned()
                .map_or_else(|| serde_json::to_vec(wire.data()).map(Bytes::from), Ok)
                .map_err(|_| middleware_protocol_error(UpstreamSendState::Sent))?,
            MiddlewareFraming::JsonDocument,
        ),
        ClientTransport::WebSocket
        | ClientTransport::InternalProbe
        | ClientTransport::InternalPlugin => (
            serde_json::to_vec(wire.data())
                .map(Bytes::from)
                .map_err(|_| middleware_protocol_error(UpstreamSendState::Sent))?,
            MiddlewareFraming::JsonDocument,
        ),
    };
    Ok(MiddlewareFrame::from_provider_event(
        bytes, framing, terminal, event,
    ))
}

fn encode_sse_wire_event(wire: &ProtocolWireEvent) -> Result<Bytes, ProviderError> {
    let mut encoded = Vec::new();
    if let Some(id) = wire.sse_id() {
        encoded.extend_from_slice(b"id: ");
        encoded.extend_from_slice(id.as_bytes());
        encoded.push(b'\n');
    }
    if let Some(retry) = wire.sse_retry() {
        encoded.extend_from_slice(format!("retry: {retry}\n").as_bytes());
    }
    if let Some(event_type) = wire.event_type() {
        encoded.extend_from_slice(b"event: ");
        encoded.extend_from_slice(event_type.as_bytes());
        encoded.push(b'\n');
    }
    encoded.extend_from_slice(b"data: ");
    serde_json::to_writer(&mut encoded, wire.data())
        .map_err(|_| middleware_protocol_error(UpstreamSendState::Sent))?;
    encoded.extend_from_slice(b"\n\n");
    Ok(Bytes::from(encoded))
}

/// 执行每次 retry 都重新建立的 attempt 中间件链。
pub async fn execute_attempt_middleware(
    plan: Option<&crate::engine::middleware::FrozenMiddlewarePlan>,
    context: MiddlewareContext,
    operation: Operation,
    transport: ClientTransport,
    terminal: ProviderMiddlewareTerminal,
) -> Result<ProviderStream, ProviderError> {
    let Some(plan) = plan else {
        return terminal(operation, Vec::new()).await;
    };
    let request = MiddlewareRequest::new(
        operation.protocol(),
        Vec::<MiddlewareHeader>::new(),
        operation
            .middleware_body()
            .map_err(|_| middleware_protocol_error(UpstreamSendState::NotSent))?,
    );
    let response = plan
        .handle(
            context,
            request,
            Box::new(ProviderMiddlewareNext {
                operation,
                transport,
                terminal: Some(terminal),
            }),
        )
        .await
        .map_err(middleware_prepare_error)?;
    let (protocol, status_code, headers, body, envelope) = response.into_parts();
    if status_code != 200 || !headers.is_empty() {
        return Err(middleware_protocol_error(UpstreamSendState::NotSent));
    }
    crate::validation::validate_text(&protocol, 64, true, None)
        .map_err(|_| middleware_protocol_error(UpstreamSendState::NotSent))?;
    let metadata = envelope
        .ok_or_else(|| middleware_protocol_error(UpstreamSendState::NotSent))?
        .into_provider_metadata();
    let events = futures::stream::try_unfold((body, protocol), |(mut body, protocol)| async move {
        let Some(frame) = body.next_frame().await.map_err(middleware_body_error)? else {
            return Ok(None);
        };
        let event = middleware_frame_to_provider_event(frame, &protocol)?;
        Ok(Some((event, (body, protocol))))
    });
    Ok(ProviderStream::new(metadata, events, ()))
}

fn middleware_frame_to_provider_event(
    frame: MiddlewareFrame,
    protocol: &str,
) -> Result<ProviderEvent, ProviderError> {
    let transformed = frame.transformed();
    let (bytes, framing, _, mut envelope) = frame.into_provider_parts();
    // 透传不能把已解析的事件降格成 raw bytes，否则 Responses 会丢失终态和 WS 帧。
    // 同时核对正文，避免进程内中间件漏标 transformed 时忽略了实际改写。
    if let Some(event) = envelope.as_ref()
        && let Some(wire) = event
            .wire_event()
            .filter(|wire| wire.protocol() == protocol)
    {
        let original = match framing {
            MiddlewareFraming::JsonDocument => wire
                .raw_json_body()
                .or_else(|| wire.raw_http_body_bytes())
                .cloned()
                .or_else(|| serde_json::to_vec(wire.data()).ok().map(Bytes::from)),
            MiddlewareFraming::SseEvent => wire
                .raw_sse_frame()
                .cloned()
                .or_else(|| encode_sse_wire_event(wire).ok()),
            MiddlewareFraming::RawBytes => wire.raw_http_body_bytes().cloned(),
        };
        if original.as_ref() == Some(&bytes)
            && let Some(mut event) = envelope.take()
        {
            if transformed {
                event.mark_middleware_transformed();
            }
            return Ok(event);
        }
    }
    let wire = if bytes.is_empty() {
        None
    } else {
        Some(
            match framing {
                MiddlewareFraming::JsonDocument => ProtocolWireEvent::raw_json(protocol, bytes),
                MiddlewareFraming::SseEvent => ProtocolWireEvent::raw_sse(protocol, bytes),
                MiddlewareFraming::RawBytes => ProtocolWireEvent::raw_http_body(protocol, bytes),
            }
            .map_err(|_| middleware_protocol_error(UpstreamSendState::Ambiguous))?,
        )
    };
    if let Some(mut event) = envelope {
        if transformed {
            event.replace_middleware_wire(wire);
        } else {
            event.replace_wire(wire);
        }
        return Ok(event);
    }
    let mut event = wire
        .map(ProviderEvent::wire)
        .ok_or_else(|| middleware_protocol_error(UpstreamSendState::Ambiguous))?;
    if transformed {
        event.mark_middleware_transformed();
    }
    Ok(event)
}

fn middleware_prepare_error(error: MiddlewareError) -> ProviderError {
    match error {
        MiddlewareError::Provider(error) => error,
        MiddlewareError::Rejected => ProviderError::new(
            ProviderErrorKind::RequestPolicyDenied,
            UpstreamSendState::NotSent,
        ),
        MiddlewareError::Fault
        | MiddlewareError::InvalidState
        | MiddlewareError::Gateway(_)
        | MiddlewareError::Engine(_) => middleware_protocol_error(UpstreamSendState::NotSent),
    }
}

fn middleware_body_error(error: MiddlewareError) -> ProviderError {
    match error {
        MiddlewareError::Provider(error) => error,
        MiddlewareError::Rejected => ProviderError::new(
            ProviderErrorKind::RequestPolicyDenied,
            UpstreamSendState::Ambiguous,
        ),
        MiddlewareError::Fault
        | MiddlewareError::InvalidState
        | MiddlewareError::Gateway(_)
        | MiddlewareError::Engine(_) => middleware_protocol_error(UpstreamSendState::Ambiguous),
    }
}

fn middleware_protocol_error(send_state: UpstreamSendState) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, send_state)
        .redact_sensitive_context("invalid middleware boundary")
}

fn native_response_protocol_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Protocol,
        crate::upstream::UpstreamSendState::Sent,
    )
    .redact_sensitive_context("invalid native response translation")
}

impl Stream for ProviderStream {
    type Item = Result<ProviderEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        if let Some(feedback) = this.account_feedback.as_mut() {
            feedback.start();
        }

        match this.events.as_mut().poll_next(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => {
                // 带 wire 的 canonical facts 只是旁路观测：wire 才是客户端协议的
                // 权威表达。只有 canonical-only Provider 输出需要以状态机作为交付条件。
                if event.wire_event().is_none() && !event.canonical_facts().is_empty() {
                    this.strict_canonical_seen = true;
                    for fact in event.canonical_facts() {
                        if this.validator.observe(fact).is_err() {
                            this.terminated = true;
                            let error = ProviderError::new(
                                ProviderErrorKind::Protocol,
                                UpstreamSendState::Sent,
                            );
                            if let Some(feedback) = this.account_feedback.as_mut() {
                                feedback.report_failure(&error);
                            }
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                }
                if let Some(feedback) = this.account_feedback.as_mut() {
                    feedback.observe(&event);
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.terminated = true;
                if let Some(feedback) = this.account_feedback.as_mut() {
                    feedback.report_failure(&error);
                }
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.terminated = true;
                let validation = if this.strict_canonical_seen {
                    this.validator.finish()
                } else {
                    Ok(())
                };
                match validation {
                    Ok(()) => {
                        if let Some(feedback) = this.account_feedback.as_mut() {
                            feedback.report_success();
                        }
                        Poll::Ready(None)
                    }
                    Err(_) => {
                        let error = ProviderError::new(
                            ProviderErrorKind::Protocol,
                            UpstreamSendState::Sent,
                        );
                        if let Some(feedback) = this.account_feedback.as_mut() {
                            feedback.report_failure(&error);
                        }
                        Poll::Ready(Some(Err(error)))
                    }
                }
            }
        }
    }
}

/// 传给 Provider 的单候选请求。
#[derive(Clone)]
pub struct ProviderRequest {
    operation: Operation,
    candidate: ProviderCandidate,
}

/// Provider 对公共观测表可解释的请求语义；未知字段保持空值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRequestObservation {
    /// 独立 Provider 端点声明的请求模型，仅用于观测，不参与文本模型目录路由。
    pub requested_model: Option<PublicModelId>,
    /// 客户端原始请求中的推理强度。
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
    pub continuation: ContinuationRequestObservation,
}

/// 仅用于恢复事件关联的客户端作用域不透明请求事实。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContinuationRequestObservation {
    pub affinity_hash: Option<String>,
    pub previous_response_id_hash: Option<String>,
    pub requested: bool,
}

impl ProviderRequest {
    /// 绑定 operation 与请求计划中冻结的 Provider 候选。
    #[must_use]
    pub const fn new(operation: Operation, candidate: ProviderCandidate) -> Self {
        Self {
            operation,
            candidate,
        }
    }

    /// 返回业务 operation。
    #[must_use]
    pub const fn operation(&self) -> &Operation {
        &self.operation
    }

    /// 返回冻结 Provider 候选。
    #[must_use]
    pub const fn candidate(&self) -> &ProviderCandidate {
        &self.candidate
    }
}

impl fmt::Debug for ProviderRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRequest")
            .field("operation", &self.operation)
            .field("provider", self.candidate.provider())
            .field("upstream_model", &self.candidate.upstream_model())
            .finish()
    }
}

/// Provider 热路径唯一接口。
///
/// 每次 `execute` 只能选择一个 credential 并准备一次可见上游调用。实现不得
/// 在内部轮换 credential 或隐藏业务 retry；失败后由 Attempt Coordinator 使用
/// 新的 attempt 再次调用。
#[async_trait]
pub trait Provider: Send + Sync {
    /// 从已冻结的配置解析请求身份；只读取本地发布资料，不执行网络请求。
    fn resolve_request_profile(
        &self,
        configuration: &crate::account::OpaqueProviderData,
    ) -> Result<crate::account::OpaqueProviderData, ProviderError> {
        Ok(configuration.clone())
    }

    /// 没有持久选择时返回 Provider 的已准备默认画像；结果仍由 Core 按请求冻结。
    fn default_request_profile(
        &self,
    ) -> Result<Option<crate::account::OpaqueProviderData>, ProviderError> {
        Ok(None)
    }

    /// 返回实例生命周期内稳定的注册名称。
    fn name(&self) -> &str;

    /// 返回当前进程已经成功发布的目录代次。
    fn catalog_generation(&self) -> ProviderCatalogGeneration;

    /// 发现型目录不用于提前拒绝上游可能支持的新模型或别名。
    fn model_catalog_is_exhaustive(&self) -> bool {
        true
    }

    /// 解释 Provider 差异化观测字段；不参与路由和传输。
    fn request_observation(
        &self,
        _operation: &Operation,
        _client_api_key_id: &ClientApiKeyId,
    ) -> ProviderRequestObservation {
        ProviderRequestObservation::default()
    }

    /// 查询当前 Provider 的实时模型目录，并由 Provider 自己编译能力事实。
    ///
    /// # Errors
    ///
    /// 目录 transport、认证或 Provider 协议失败时返回稳定错误。
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError>;

    /// 读取当前客户端协议的原生目录，必须限定到认证时冻结的账号范围。
    /// `None` 表示不提供该协议的原生目录；读取失败不能伪装成不支持。
    async fn query_client_model_catalog(
        &self,
        _scope: &crate::account::scope::FrozenAccountScope,
        _protocol: &str,
        _client_version: &str,
    ) -> Result<
        Option<Vec<crate::routing::ProviderModelDescriptor>>,
        crate::routing::ProviderCatalogUnavailable,
    > {
        Ok(None)
    }

    /// 选择一个未被排除的资源并返回 cold [`ProviderStream`]。
    ///
    /// 返回成功、返回错误或准备 future 被取消前，均不得发送本次请求的上游握手或业务
    /// 载荷，也不得启动可独立完成这些发送的后台任务。发送只在返回的流被 poll 后发生，
    /// 保证 Core 能在真实出站前校验账号范围并登记 attempt。
    ///
    /// # Errors
    ///
    /// 没有可用资源、请求无效或准备失败时返回 `NotSent` 错误；
    /// 可能已发送的失败必须通过 stream 返回，不得降级发送事实。
    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError>;
}

/// Provider Registry 构建错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    /// Provider 名称无效。
    #[error("invalid provider registry name")]
    InvalidName,
    /// Provider 重复注册。
    #[error("provider `{provider}` is already registered")]
    Duplicate {
        /// Provider 名称。
        provider: String,
    },
}

/// 唯一保存 `Arc<dyn Provider>` 的异构注册表。
#[derive(Default)]
pub struct ProviderRegistryBuilder {
    providers: BTreeMap<ProviderKind, Arc<dyn Provider>>,
}

impl ProviderRegistryBuilder {
    /// 创建空 builder。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    /// 注册 Provider。
    ///
    /// # Errors
    ///
    /// 名称无效或重复时返回错误。
    pub fn register(&mut self, provider: Arc<dyn Provider>) -> Result<(), RegistryError> {
        let kind = ProviderKind::new(provider.name()).map_err(|_| RegistryError::InvalidName)?;
        if self.providers.contains_key(&kind) {
            return Err(RegistryError::Duplicate {
                provider: kind.as_str().to_owned(),
            });
        }
        self.providers.insert(kind, provider);
        Ok(())
    }

    /// 冻结注册表。
    #[must_use]
    pub fn build(self) -> ProviderRegistry {
        ProviderRegistry {
            providers: Arc::new(self.providers),
        }
    }
}

/// 固定内置 Provider 的不可变注册表。
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    pub(super) providers: Arc<BTreeMap<ProviderKind, Arc<dyn Provider>>>,
}

impl ProviderRegistry {
    /// 从一组异构 Provider 构造冻结注册表。
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn Provider>>,
    ) -> Result<Self, RegistryError> {
        let mut builder = ProviderRegistryBuilder::new();
        for provider in providers {
            builder.register(provider)?;
        }
        Ok(builder.build())
    }

    /// 创建 builder。
    #[must_use]
    pub const fn builder() -> ProviderRegistryBuilder {
        ProviderRegistryBuilder::new()
    }

    /// 按 Provider 名称查询 adapter。
    #[must_use]
    pub fn get(&self, provider: &ProviderKind) -> Option<&Arc<dyn Provider>> {
        self.providers.get(provider)
    }

    #[must_use]
    pub fn request_observation(
        &self,
        provider: &ProviderKind,
        operation: &Operation,
        client_api_key_id: &ClientApiKeyId,
    ) -> ProviderRequestObservation {
        self.providers
            .get(provider)
            .map_or_else(ProviderRequestObservation::default, |registered| {
                registered.request_observation(operation, client_api_key_id)
            })
    }

    /// 判断 Provider 是否已注册。
    #[must_use]
    pub fn contains(&self, provider: &ProviderKind) -> bool {
        self.providers.contains_key(provider)
    }

    /// 返回注册数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// 判断注册表是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl ProviderCatalogPort for ProviderRegistry {
    fn model_catalog_is_exhaustive(&self, provider: &ProviderKind) -> bool {
        self.providers
            .get(provider)
            .is_none_or(|provider| provider.model_catalog_is_exhaustive())
    }

    fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        self.providers
            .iter()
            .map(|(kind, provider)| (kind.clone(), provider.catalog_generation()))
            .collect()
    }

    fn query_model_capabilities(
        &self,
        provider_kind: &ProviderKind,
    ) -> BoxFuture<'_, Result<Vec<ProviderModelCapabilities>, ProviderCatalogUnavailable>> {
        let provider = self.providers.get(provider_kind);
        Box::pin(async move {
            provider
                .ok_or(ProviderCatalogUnavailable)?
                .query_model_capabilities()
                .await
                .map_err(|_| ProviderCatalogUnavailable)
        })
    }
}
