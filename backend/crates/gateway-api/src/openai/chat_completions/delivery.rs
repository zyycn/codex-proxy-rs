//! Chat 出口持有协议投影缓冲 提交与资源回收始终委托原受管正文

use std::collections::VecDeque;

use axum::body::to_bytes;
use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::{
    engine::middleware::{
        MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareFraming, MiddlewareHeader,
        MiddlewareResponse,
    },
    error::{GatewayError, GatewayErrorKind},
};
use gateway_protocol::openai::sse::{DONE_SSE_FRAME, SseEvent, SseEventDecoder, sse_frame_is_done};
use serde_json::Value;

use crate::openai::{middleware::error_response, responses::ProtocolErrorBody};

use super::response::{ChatIdentity, ChatStreamEncoder, project_response};

pub(super) fn project_middleware_response(
    response: MiddlewareResponse,
    identity: ChatIdentity,
    streaming: bool,
    include_usage: bool,
) -> MiddlewareResponse {
    if !(200..300).contains(&response.status_code()) {
        return response;
    }
    let (protocol, status, mut headers, body, envelope) = response.into_parts();
    // 投影改变正文长度和表示形式 不继承 Responses 的实体长度或编码
    headers.retain(|header| {
        !["content-length", "content-type", "content-encoding"]
            .iter()
            .any(|name| header.name().eq_ignore_ascii_case(name))
    });
    headers.push(MiddlewareHeader::new(
        "content-type",
        Bytes::from_static(if streaming {
            b"text/event-stream"
        } else {
            b"application/json"
        }),
    ));
    let projection = if streaming {
        Projection::Stream(Box::new(StreamProjection {
            encoder: ChatStreamEncoder::new(identity, include_usage),
            decoder: SseEventDecoder::default(),
            done_received: false,
        }))
    } else {
        Projection::Json(identity)
    };
    let mut response = MiddlewareResponse::new(
        protocol,
        status,
        headers,
        Box::new(ChatBody {
            inner: Some(body),
            projection,
            pending: VecDeque::new(),
            committed: false,
            finished: false,
        }),
    );
    if let Some(envelope) = envelope {
        response = response.with_envelope(envelope);
    }
    response
}

pub(super) fn chat_error_frame(data: &Value) -> Bytes {
    let error = data
        .pointer("/response/error")
        .or_else(|| data.get("error"));
    Bytes::from(format!("data: {}\n\n", serde_json::json!({"error": error})))
}

enum Projection {
    Json(ChatIdentity),
    Stream(Box<StreamProjection>),
}

impl Projection {
    fn terminal_seen(&self) -> bool {
        matches!(self, Self::Stream(stream) if stream.terminal_seen())
    }
}

struct StreamProjection {
    encoder: ChatStreamEncoder,
    decoder: SseEventDecoder,
    done_received: bool,
}

impl StreamProjection {
    fn terminal_seen(&self) -> bool {
        self.encoder.is_completed() || self.encoder.has_wire_failure()
    }

    fn project(&mut self, events: Vec<SseEvent>) -> Result<Vec<Bytes>, MiddlewareError> {
        let mut output = Vec::new();
        for event in events {
            if self.done_received {
                return Err(invalid_response());
            }
            let data = serde_json::from_str(&event.data).map_err(|_| invalid_response())?;
            // codec 区分辅助事件与终态后的业务输出 交付层不再维护一份事件分类
            output.extend(
                self.encoder
                    .push_event(event.event.as_deref(), &data)
                    .map_err(projection_error)?,
            );
        }
        Ok(output)
    }
}

struct ChatBody {
    inner: Option<Box<dyn MiddlewareBody>>,
    projection: Projection,
    pending: VecDeque<MiddlewareFrame>,
    committed: bool,
    finished: bool,
}

impl ChatBody {
    async fn read_one(&mut self) -> Result<(), MiddlewareError> {
        let inner = self.inner.as_mut().ok_or(MiddlewareError::InvalidState)?;
        let Some(frame) = inner.next_frame().await? else {
            if let Projection::Stream(stream) = &mut self.projection {
                let events = stream.decoder.finish().map_err(|_| invalid_response())?;
                let frames = stream.project(events)?;
                if !frames.is_empty() || !stream.done_received || !inner.is_finalized() {
                    return Err(invalid_response());
                }
                self.pending.push_back(done_frame());
            }
            self.finished = true;
            return Ok(());
        };
        let transformed = frame.transformed();
        let (bytes, framing, terminal, envelope) = frame.into_parts();
        let output = match &mut self.projection {
            Projection::Json(identity) => {
                if framing != MiddlewareFraming::JsonDocument || !terminal {
                    return Err(invalid_response());
                }
                let value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
                let value = project_response(&value, identity).map_err(projection_error)?;
                vec![MiddlewareFrame::new(
                    Bytes::from(serde_json::to_vec(&value).map_err(|_| invalid_response())?),
                    MiddlewareFraming::JsonDocument,
                    true,
                )]
            }
            Projection::Stream(stream) => {
                if framing != MiddlewareFraming::SseEvent {
                    return Err(invalid_response());
                }
                let events = stream
                    .decoder
                    .push(&bytes)
                    .map_err(|_| invalid_response())?;
                let output = stream.project(events)?;
                // 共用 SSE 解码器只返回业务事件 DONE 由受管正文的控制帧单独确认
                if std::str::from_utf8(&bytes).is_ok_and(sse_frame_is_done) {
                    if stream.done_received || !stream.terminal_seen() {
                        return Err(invalid_response());
                    }
                    stream.done_received = true;
                }
                if terminal && !stream.done_received {
                    return Err(invalid_response());
                }
                output
                    .into_iter()
                    .map(|bytes| MiddlewareFrame::new(bytes, MiddlewareFraming::SseEvent, false))
                    .collect()
            }
        };
        let mut output: VecDeque<_> = output
            .into_iter()
            .map(|frame| frame.with_transformed(transformed))
            .collect();
        if let Some(envelope) = envelope
            && let Some(last) = output.pop_back()
        {
            output.push_back(last.with_envelope(envelope));
        }
        self.pending.extend(output);
        Ok(())
    }

    async fn verify_terminal(&mut self) -> Result<(), MiddlewareError> {
        while !self.finished {
            self.read_one().await?;
        }
        Ok(())
    }

    async fn fail_stream(&mut self, error: MiddlewareError) {
        self.pending.clear();
        if let Some(inner) = self.inner.take() {
            inner.close().await;
        }
        let response = error_response(error);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::json!({"error": {
                "type": "server_error", "code": "invalid_upstream_response",
                "message": "The gateway could not deliver the Chat response."
            }})
        });
        self.pending.push_back(MiddlewareFrame::new(
            chat_error_frame(&value),
            MiddlewareFraming::SseEvent,
            false,
        ));
        self.pending.push_back(done_frame());
        self.finished = true;
    }
}

impl MiddlewareBody for ChatBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            while self.pending.is_empty() && !self.finished {
                let result = match self.read_one().await {
                    Ok(()) if self.committed && self.projection.terminal_seen() => {
                        self.verify_terminal().await
                    }
                    result => result,
                };
                if let Err(error) = result {
                    if !self.committed {
                        return Err(error);
                    }
                    self.fail_stream(error).await;
                }
            }
            Ok(self.pending.pop_front())
        })
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            self.inner
                .as_mut()
                .ok_or(MiddlewareError::InvalidState)?
                .commit_downstream(client_status_code)
                .await?;
            self.committed = true;
            // HTTP 尚未发送首帧 即使首批已经包含 finish 或 usage 也先确认原会话完成结算
            if self.projection.terminal_seen()
                && let Err(error) = self.verify_terminal().await
            {
                // 原会话已提交 不能把已记录的 200 改成另一 HTTP 状态
                // 首个 Chat 帧为 role 或 error 成功终态仍在 pending 内可安全撤回
                self.fail_stream(error).await;
            }
            Ok(())
        })
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        match self.inner.as_mut() {
            Some(inner) => inner.record_client_status(status),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn is_finalized(&self) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.is_finalized())
    }

    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        let inner = self.inner.take();
        Box::pin(async move {
            if let Some(inner) = inner {
                inner.close().await;
            }
        })
    }
}

fn projection_error(error: ProtocolErrorBody) -> MiddlewareError {
    MiddlewareError::Gateway(
        GatewayError::new(
            GatewayErrorKind::UpstreamUnavailable,
            "The gateway could not project the upstream response to Chat Completions.",
        )
        .with_client_code(error.error.code),
    )
}

fn invalid_response() -> MiddlewareError {
    MiddlewareError::Gateway(
        GatewayError::new(
            GatewayErrorKind::UpstreamUnavailable,
            "The gateway could not deliver the Chat response.",
        )
        .with_client_code("invalid_upstream_response"),
    )
}

fn done_frame() -> MiddlewareFrame {
    MiddlewareFrame::new(
        Bytes::from_static(DONE_SSE_FRAME.as_bytes()),
        MiddlewareFraming::SseEvent,
        true,
    )
}
