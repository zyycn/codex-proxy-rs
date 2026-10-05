//! 插件 HTTP 中间件的类型化请求、响应、正文流与后续调用接口

use std::{collections::VecDeque, future::Future, pin::Pin};

use super::super::session::{
    CallCancellation, CallReply, HostClient, PluginCall, PullResponseFuture, PullResponseStream,
    ResponseStream, SessionError,
};
use super::{MiddlewareInput, MiddlewareOutput, invalid_input};
use crate::{
    CallContext, PluginFault, Stage,
    call::middleware::{
        HANDLE_METHOD, MiddlewareBodyClose, MiddlewareBodyRead, MiddlewareHeader, NEXT_METHOD,
        http as wire,
    },
    client::{HostReply, read::PendingRead},
};

/// HTTP 数据帧与 trailers 分开表达；结束由流完成表示，不插入协议正文
pub enum HttpFrame {
    Data(Vec<u8>),
    Trailers(Vec<MiddlewareHeader>),
}

impl HttpFrame {
    fn encode(self) -> Result<Vec<u8>, PluginFault> {
        match self {
            Self::Data(data) => {
                let mut bytes = Vec::with_capacity(1 + data.len());
                bytes.push(0);
                bytes.extend(data);
                Ok(bytes)
            }
            Self::Trailers(headers) => {
                let mut bytes = vec![1];
                serde_json::to_writer(&mut bytes, &headers).map_err(|_| invalid_input())?;
                Ok(bytes)
            }
        }
    }
}

/// HTTP 正文惰性读取；透传直接返还句柄，完整收集由插件自行决定
pub struct HttpBody(Source);

type Producer = Pin<Box<dyn Future<Output = Result<(), PluginFault>> + Send>>;

enum Source {
    Empty,
    Bytes(Vec<u8>),
    Host {
        handle: String,
        host: HostClient,
        read: PendingRead<Result<HostReply, SessionError>>,
        pending: VecDeque<u8>,
    },
    Stream(ResponseStream),
    Producing {
        body: Box<HttpBody>,
        producer: Option<Producer>,
    },
}

impl HttpBody {
    #[must_use]
    pub const fn empty() -> Self {
        Self(Source::Empty)
    }
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(Source::Bytes(bytes))
    }

    pub async fn read(&mut self) -> Result<Option<HttpFrame>, PluginFault> {
        match &mut self.0 {
            Source::Empty => return Ok(None),
            Source::Bytes(bytes) => {
                let bytes = std::mem::take(bytes);
                self.0 = Source::Empty;
                return Ok(Some(HttpFrame::Data(bytes)));
            }
            Source::Stream(stream) => {
                return stream
                    .next()
                    .await
                    .transpose()?
                    .map(|bytes| match bytes.split_first() {
                        Some((0, data)) => Ok(HttpFrame::Data(data.to_vec())),
                        Some((1, data)) => serde_json::from_slice(data)
                            .map(HttpFrame::Trailers)
                            .map_err(|_| invalid_input()),
                        _ => Err(invalid_input()),
                    })
                    .transpose();
            }
            Source::Producing { body, producer } => {
                let read = Box::pin(body.read());
                let Some(active) = producer.as_mut() else {
                    return read.await;
                };
                tokio::pin!(read);
                return tokio::select! {
                    biased;
                    frame = &mut read => frame,
                    produced = active => { produced?; producer.take(); read.await }
                };
            }
            Source::Host { .. } => {}
        }
        let Source::Host {
            handle,
            host,
            read,
            pending,
        } = &mut self.0
        else {
            return Err(invalid_input());
        };
        if !pending.is_empty() {
            let count = pending
                .len()
                .min(host.maximum_stream_chunk_bytes().saturating_sub(1));
            if count == 0 {
                return Err(invalid_input());
            }
            return Ok(Some(HttpFrame::Data(pending.drain(..count).collect())));
        }
        let reply = read
            .run(|| {
                let host = host.clone();
                let handle = handle.clone();
                async move {
                    let maximum_bytes =
                        u32::try_from(host.maximum_stream_chunk_bytes().saturating_sub(1))
                            .map_err(|_| SessionError::Protocol)?;
                    host.call(
                        wire::BODY_READ_METHOD,
                        serde_json::to_value(MiddlewareBodyRead {
                            handle,
                            maximum_bytes,
                        })
                        .map_err(|_| SessionError::Protocol)?,
                        Vec::new(),
                    )
                    .await
                }
            })
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let read: wire::BodyRead =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        if read.eof {
            if !reply.payload.is_empty() || read.trailers.is_some() {
                return Err(invalid_input());
            }
            self.0 = Source::Empty;
            return Ok(None);
        }
        match read.trailers {
            Some(headers) if reply.payload.is_empty() => Ok(Some(HttpFrame::Trailers(headers))),
            Some(_) => Err(invalid_input()),
            None => {
                // 读取可能早于初始 Credit 发出；返回路径仍须遵守实际窗口，并为帧标记留一字节
                let maximum = host.maximum_stream_chunk_bytes().saturating_sub(1);
                if maximum == 0 {
                    return Err(invalid_input());
                }
                if reply.payload.len() <= maximum {
                    Ok(Some(HttpFrame::Data(reply.payload)))
                } else {
                    *pending = reply.payload.into();
                    Ok(Some(HttpFrame::Data(pending.drain(..maximum).collect())))
                }
            }
        }
    }

    pub async fn close(&mut self) -> Result<(), PluginFault> {
        if let Source::Producing { body, .. } = &mut self.0 {
            Box::pin(body.close()).await?;
        } else if let Source::Host { handle, host, .. } = &self.0 {
            let reply = host
                .call(
                    wire::BODY_CLOSE_METHOD,
                    serde_json::to_value(MiddlewareBodyClose {
                        handle: handle.clone(),
                    })
                    .map_err(|_| invalid_input())?,
                    Vec::new(),
                )
                .await
                .map_err(SessionError::into_plugin_fault)?;
            if reply.result != serde_json::json!({}) || !reply.payload.is_empty() {
                return Err(invalid_input());
            }
        }
        self.0 = Source::Empty;
        Ok(())
    }

    /// 按输出背压逐帧转换；None 丢弃一帧
    /// 需要展开时可使用自定义 ResponseStream
    pub fn map_frames<F>(self, transform: F) -> Self
    where
        F: FnMut(HttpFrame) -> Result<Option<HttpFrame>, PluginFault> + Send + 'static,
    {
        Self(Source::Stream(ResponseStream::pull(Box::new(MappedBody {
            body: self,
            transform,
        }))))
    }

    fn from_wire(body: wire::Body, host: HostClient) -> Result<Self, PluginFault> {
        match body {
            wire::Body::Empty => Ok(Self::empty()),
            wire::Body::Handle { handle } if !handle.is_empty() => Ok(Self(Source::Host {
                handle,
                host,
                read: PendingRead::default(),
                pending: VecDeque::new(),
            })),
            _ => Err(invalid_input()),
        }
    }

    fn into_wire(self) -> (wire::Body, Vec<u8>, Option<ResponseStream>) {
        match self.0 {
            Source::Empty => (wire::Body::Empty, Vec::new(), None),
            Source::Bytes(bytes) => (wire::Body::Bytes, bytes, None),
            Source::Host {
                handle,
                host,
                read,
                pending,
            } => {
                if read.is_pending() || !pending.is_empty() {
                    (
                        wire::Body::Stream,
                        Vec::new(),
                        Some(ResponseStream::pull(Box::new(ForwardBody(Self(
                            Source::Host {
                                handle,
                                host,
                                read,
                                pending,
                            },
                        ))))),
                    )
                } else {
                    (wire::Body::Handle { handle }, Vec::new(), None)
                }
            }
            Source::Stream(stream) => (wire::Body::Stream, Vec::new(), Some(stream)),
            Source::Producing { body, producer } => (
                wire::Body::Stream,
                Vec::new(),
                Some(ResponseStream::pull(Box::new(ForwardBody(HttpBody(
                    Source::Producing { body, producer },
                ))))),
            ),
        }
    }
}

struct ForwardBody(HttpBody);
impl PullResponseStream for ForwardBody {
    fn next(&mut self) -> PullResponseFuture<'_> {
        Box::pin(async {
            self.0
                .read()
                .await
                .and_then(|frame| frame.map(HttpFrame::encode).transpose())
                .transpose()
        })
    }
}

async fn produce(
    mut stream: ResponseStream,
    host: HostClient,
    handle: String,
) -> Result<(), PluginFault> {
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let (trailers, payload) = match chunk.split_first() {
            Some((0, data)) => (None, data.to_vec()),
            Some((1, data)) => (
                Some(serde_json::from_slice(data).map_err(|_| invalid_input())?),
                Vec::new(),
            ),
            _ => return Err(invalid_input()),
        };
        if !write_body(
            &host,
            wire::BodyWrite {
                handle: handle.clone(),
                eof: false,
                trailers,
            },
            payload,
        )
        .await?
        {
            return Ok(());
        }
    }
    write_body(
        &host,
        wire::BodyWrite {
            handle,
            eof: true,
            trailers: None,
        },
        Vec::new(),
    )
    .await?;
    Ok(())
}

async fn write_body(
    host: &HostClient,
    write: wire::BodyWrite,
    payload: Vec<u8>,
) -> Result<bool, PluginFault> {
    match host
        .call(
            wire::BODY_WRITE_METHOD,
            serde_json::to_value(write).map_err(|_| invalid_input())?,
            payload,
        )
        .await
    {
        Ok(reply) if reply.result == serde_json::json!({}) && reply.payload.is_empty() => Ok(true),
        // 下游允许在读取完整请求前返回，例如拒绝认证或直接响应
        Err(SessionError::Remote(fault)) if fault.code == crate::ErrorCode::Conflict => Ok(false),
        Err(error) => Err(error.into_plugin_fault()),
        _ => Err(invalid_input()),
    }
}

struct MappedBody<F> {
    body: HttpBody,
    transform: F,
}
impl<F> PullResponseStream for MappedBody<F>
where
    F: FnMut(HttpFrame) -> Result<Option<HttpFrame>, PluginFault> + Send + 'static,
{
    fn next(&mut self) -> PullResponseFuture<'_> {
        Box::pin(async move {
            async {
                while let Some(frame) = self.body.read().await? {
                    if let Some(frame) = (self.transform)(frame)? {
                        return frame.encode().map(Some);
                    }
                }
                Ok(None)
            }
            .await
            .transpose()
        })
    }
}

pub struct HttpRequest {
    pub settings: serde_json::Value,
    pub method: String,
    pub uri: String,
    pub version: wire::Version,
    pub headers: Vec<MiddlewareHeader>,
    pub timeout_ms: Option<u64>,
    pub body: HttpBody,
}

pub struct HttpNext {
    host: HostClient,
}
impl HttpNext {
    pub async fn run(self, request: HttpRequest) -> Result<HttpResponse, PluginFault> {
        send_request(&self.host, NEXT_METHOD, request).await
    }
}

impl HostClient {
    /// 主动调用宿主相对路径，经过完整 HTTP 洋葱链并继承父调用的取消与防递归上下文
    pub async fn dispatch_http(&self, request: HttpRequest) -> Result<HttpResponse, PluginFault> {
        send_request(self, wire::DISPATCH_METHOD, request).await
    }
}

async fn send_request(
    host: &HostClient,
    method: &str,
    request: HttpRequest,
) -> Result<HttpResponse, PluginFault> {
    let (mut body, payload, stream) = request.body.into_wire();
    let mut producer: Option<Producer> = if let Some(stream) = stream {
        let reply = host
            .call(wire::BODY_CREATE_METHOD, serde_json::json!({}), Vec::new())
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(invalid_input());
        }
        let pipe: wire::BodyPipe =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        body = wire::Body::Handle {
            handle: pipe.handle.clone(),
        };
        Some(Box::pin(produce(stream, host.clone(), pipe.handle)))
    } else {
        None
    };
    let request = wire::Request {
        settings: request.settings,
        method: request.method,
        uri: request.uri,
        version: request.version,
        headers: request.headers,
        timeout_ms: request.timeout_ms,
        body,
    };
    let params = serde_json::to_value(request).map_err(|_| invalid_input())?;
    let next = host.call(method, params, payload);
    tokio::pin!(next);
    let reply = if let Some(active) = producer.as_mut() {
        tokio::select! {
            biased;
            result = &mut next => result,
            produced = active => { produced?; producer.take(); next.await }
        }
    } else {
        next.await
    }
    .map_err(SessionError::into_plugin_fault)?;
    if !reply.payload.is_empty() {
        return Err(invalid_input());
    }
    let response: wire::Response =
        serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
    Ok(HttpResponse {
        status: response.status,
        version: response.version,
        headers: response.headers,
        body: {
            let body = HttpBody::from_wire(response.body, host.clone())?;
            if producer.is_some() {
                HttpBody(Source::Producing {
                    body: Box::new(body),
                    producer,
                })
            } else {
                body
            }
        },
        source: response.response,
        session: None,
    })
}

pub struct HttpResponse {
    pub status: u16,
    pub version: wire::Version,
    pub headers: Vec<MiddlewareHeader>,
    pub body: HttpBody,
    source: Option<String>,
    session: Option<Producer>,
}

impl HttpResponse {
    #[must_use]
    pub fn new(status: u16, body: HttpBody) -> Self {
        Self {
            status,
            version: wire::Version::Http11,
            headers: Vec::new(),
            body,
            source: None,
            session: None,
        }
    }

    pub fn into_reply(self) -> Result<CallReply, PluginFault> {
        let (body, payload, stream) = self.body.into_wire();
        let response = wire::Response {
            status: self.status,
            version: self.version,
            headers: self.headers,
            body,
            response: self.source,
            session: self.session.is_some(),
        };
        Ok(CallReply::stream(
            serde_json::to_value(response).map_err(|_| invalid_input())?,
            payload,
            self.session
                .map(|task| ResponseStream::pull(Box::new(SessionTask(Some(task)))))
                .or(stream)
                .unwrap_or_else(|| ResponseStream::from_chunks(Vec::new())),
        ))
    }
}

pub struct HttpCall {
    pub settings_sources: serde_json::Value,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub context: CallContext,
    pub request: HttpRequest,
    pub next: HttpNext,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}

struct SessionTask(Option<Producer>);
impl PullResponseStream for SessionTask {
    fn next(&mut self) -> PullResponseFuture<'_> {
        Box::pin(async move { self.0.take()?.await.err().map(Err) })
    }
}

impl HttpCall {
    /// 在当前 HTTP 返回路径选择握手；处理函数随真实连接运行，完成或失败时回收连接
    pub async fn upgrade<F, Fut>(
        self,
        protocols: Vec<String>,
        handler: F,
    ) -> Result<HttpResponse, PluginFault>
    where
        F: FnOnce(super::websocket::WebSocketSession) -> Fut,
        Fut: Future<Output = Result<(), PluginFault>> + Send + 'static,
    {
        let request = wire::Request {
            settings: self.request.settings,
            method: self.request.method,
            uri: self.request.uri,
            version: self.request.version,
            headers: self.request.headers,
            timeout_ms: self.request.timeout_ms,
            body: wire::Body::Empty,
        };
        let reply = self
            .host
            .call(
                wire::UPGRADE_METHOD,
                serde_json::to_value(wire::Upgrade { request, protocols })
                    .map_err(|_| invalid_input())?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(invalid_input());
        }
        let response: wire::Response =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        let session = super::websocket::WebSocketSession::new(self.host, self.cancellation);
        Ok(HttpResponse {
            status: response.status,
            version: response.version,
            headers: response.headers,
            body: HttpBody::empty(),
            source: response.response,
            session: Some(Box::pin(handler(session))),
        })
    }

    pub fn try_from(call: PluginCall) -> Result<Self, PluginFault> {
        if call.method != HANDLE_METHOD
            || call.context.stage != Stage::Http
            || !call.payload.is_empty()
        {
            return Err(invalid_input());
        }
        let input: wire::Call = serde_json::from_value(call.params).map_err(|_| invalid_input())?;
        if call.context.request_id.as_ref() != Some(&input.request_id) {
            return Err(invalid_input());
        }
        let request = input.request;
        Ok(Self {
            settings_sources: input.settings_sources,
            call_id: input.call_id,
            parent_call_id: input.parent_call_id,
            context: call.context,
            cancellation: call.cancellation,
            request: HttpRequest {
                settings: request.settings,
                method: request.method,
                uri: request.uri,
                version: request.version,
                headers: request.headers,
                timeout_ms: request.timeout_ms,
                body: HttpBody::from_wire(request.body, call.host.clone())?,
            },
            next: HttpNext {
                host: call.host.clone(),
            },
            host: call.host,
        })
    }
}

impl MiddlewareInput for HttpCall {
    type Output = HttpResponse;
    fn accepts(stage: Stage) -> bool {
        stage == Stage::Http
    }
    fn decode(call: PluginCall) -> Result<Self, PluginFault> {
        Self::try_from(call)
    }
}
impl MiddlewareOutput for HttpResponse {
    fn encode(self) -> Result<CallReply, PluginFault> {
        self.into_reply()
    }
}
