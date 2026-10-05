//! 未提交响应的 HTTP 交付；只验证协议边界，不解释或重算 Core 的业务事实

use crate::middleware::headers::{decode_headers, encode_headers};
use crate::openai::error::{
    gateway_error_from_engine, gateway_error_response, openai_error_response,
};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use bytes::Bytes;
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::{
    engine::middleware::{
        MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareFraming, MiddlewareHeader,
        MiddlewareResponse,
    },
    error::GatewayError,
};

/// 只读目录/用量与协议错误共用的单帧响应；没有 Provider attempt 或计量副作用
pub(crate) async fn buffered_response(
    protocol: &str,
    response: Response,
) -> Result<MiddlewareResponse, MiddlewareError> {
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| MiddlewareError::Fault)?;
    let framing = if parts.status.is_success() {
        MiddlewareFraming::JsonDocument
    } else {
        MiddlewareFraming::RawBytes
    };
    Ok(MiddlewareResponse::new(
        protocol.to_owned(),
        parts.status.as_u16(),
        encode_headers(&parts.headers),
        Box::new(BufferedBody(Some(MiddlewareFrame::new(
            bytes, framing, true,
        )))),
    ))
}

struct BufferedBody(Option<MiddlewareFrame>);

impl MiddlewareBody for BufferedBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async { Ok(self.0.take()) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { drop(self) })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ExpectedBody {
    SingleJson,
    SinglePayload,
    Sse,
}

impl ExpectedBody {
    fn validate(self, frame: &MiddlewareFrame) -> Result<(), MiddlewareError> {
        let valid = match self {
            Self::SingleJson => {
                frame.framing() == MiddlewareFraming::JsonDocument
                    && frame.terminal()
                    && serde_json::from_slice::<serde::de::IgnoredAny>(frame.bytes()).is_ok()
            }
            // 上游错误正文可能不是 JSON；只限定单文档交付，不改写原始错误字节
            Self::SinglePayload => {
                frame.terminal()
                    && (frame.framing() == MiddlewareFraming::RawBytes
                        || Self::SingleJson.validate(frame).is_ok())
            }
            // 原生超大 SSE 帧允许按原字节片段交付；插件写入边界由 Runtime 校验
            Self::Sse => frame.framing() == MiddlewareFraming::SseEvent,
        };
        if valid {
            Ok(())
        } else {
            Err(MiddlewareError::InvalidState)
        }
    }
}

/// 全部返回包装完成、首帧可读取后才提交原有 Core delivery barrier
pub(crate) async fn into_http_response(
    response: MiddlewareResponse,
    expected: ExpectedBody,
) -> Response {
    let (protocol, status, headers, mut body, _) = response.into_parts();
    if protocol != "openai" {
        return fail_before_commit(body, MiddlewareError::InvalidState).await;
    }
    let head = response_head(status, headers);
    let (status, mut headers) = match head {
        Ok(head) => head,
        Err(error) => return fail_before_commit(body, error).await,
    };
    let first = match body.next_frame().await {
        Ok(first) => first,
        Err(error) => return fail_before_commit(body, error).await,
    };
    let empty = matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED);
    let expected = if status.is_success() {
        expected
    } else {
        ExpectedBody::SinglePayload
    };
    let validation = match (&first, empty) {
        (None, true) => Ok(()),
        (Some(frame), false) => expected.validate(frame),
        _ => Err(MiddlewareError::InvalidState),
    };
    if let Err(error) = validation {
        return fail_before_commit(body, error).await;
    }
    if let Some(frame) = &first {
        // 短路响应没有下游 adapter 生成的头，传输类型仍由宿主按帧合同补齐
        headers.entry(header::CONTENT_TYPE).or_insert_with(|| {
            HeaderValue::from_static(match frame.framing() {
                MiddlewareFraming::JsonDocument => "application/json",
                MiddlewareFraming::SseEvent => "text/event-stream",
                MiddlewareFraming::RawBytes => "application/octet-stream",
            })
        });
    }
    if matches!(expected, ExpectedBody::Sse) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, no-transform"),
        );
        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
    let exhausted = empty || !matches!(expected, ExpectedBody::Sse);
    if !empty && exhausted {
        match body.next_frame().await {
            Ok(None) => {}
            Ok(Some(_)) => return fail_before_commit(body, MiddlewareError::InvalidState).await,
            Err(error) => return fail_before_commit(body, error).await,
        }
    }
    if let Err(error) = body.commit_downstream(Some(status.as_u16())).await {
        return fail_before_commit(body, error).await;
    }
    let delivery = HttpDelivery {
        first,
        body,
        expected,
        exhausted,
        terminal_seen: empty,
    };
    let keep_alive = matches!(expected, ExpectedBody::Sse);
    let stream = Box::pin(futures::stream::try_unfold(delivery, HttpDelivery::next));
    // 固定同一条输出流；心跳不能取消并重建正在等待的 next_frame 或结算 future
    let stream = futures::stream::unfold(stream, move |mut stream| async move {
        let chunk = tokio::select! {
            biased;
            chunk = stream.next() => chunk?,
            () = tokio::time::sleep(std::time::Duration::from_secs(15)), if keep_alive => {
                Ok(Bytes::from_static(b": keep-alive\n\n"))
            }
        };
        Some((chunk, stream))
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

struct HttpDelivery {
    first: Option<MiddlewareFrame>,
    body: Box<dyn MiddlewareBody>,
    expected: ExpectedBody,
    exhausted: bool,
    terminal_seen: bool,
}

impl HttpDelivery {
    async fn next(mut self) -> Result<Option<(Bytes, Self)>, MiddlewareError> {
        let frame = match self.first.take() {
            Some(frame) => Some(frame),
            None if self.exhausted => None,
            None => match self.body.next_frame().await {
                Ok(frame) => frame,
                Err(error) => {
                    self.body.close().await;
                    return Err(error);
                }
            },
        };
        if let Some(frame) = frame {
            if self.terminal_seen || self.expected.validate(&frame).is_err() {
                self.body.close().await;
                return Err(MiddlewareError::InvalidState);
            }
            self.terminal_seen = frame.terminal();
            Ok(Some((frame.into_bytes(), self)))
        } else {
            self.body.close().await;
            if self.terminal_seen {
                Ok(None)
            } else {
                Err(MiddlewareError::InvalidState)
            }
        }
    }
}

fn response_head(
    status: u16,
    headers: Vec<MiddlewareHeader>,
) -> Result<(StatusCode, HeaderMap), MiddlewareError> {
    let status = StatusCode::from_u16(status).map_err(|_| MiddlewareError::InvalidState)?;
    if status.is_informational() {
        return Err(MiddlewareError::InvalidState);
    }
    // 原生上游头已在协议终端处理；插件返回后的字段只做 HTTP 类型校验
    Ok((status, decode_headers(headers)?))
}

async fn fail_before_commit(mut body: Box<dyn MiddlewareBody>, error: MiddlewareError) -> Response {
    let response = error_response(error);
    let _ = body.record_client_status(response.status().as_u16()).await;
    body.close().await;
    response
}

pub(crate) fn error_response(error: MiddlewareError) -> Response {
    match error {
        MiddlewareError::Gateway(error) => gateway_error_response(&error),
        MiddlewareError::Engine(error) => {
            gateway_error_response(&gateway_error_from_engine(&error))
        }
        MiddlewareError::Provider(error) => {
            gateway_error_response(&GatewayError::from_provider(&error))
        }
        MiddlewareError::Rejected | MiddlewareError::Remote { rejected: true, .. } => {
            openai_error_response(
                StatusCode::FORBIDDEN,
                "Request rejected by middleware",
                "invalid_request_error",
                "middleware_rejected",
            )
            .into_response()
        }
        MiddlewareError::Fault | MiddlewareError::Remote { .. } | MiddlewareError::InvalidState => {
            openai_error_response(
                StatusCode::BAD_GATEWAY,
                "Request middleware failed",
                "server_error",
                "middleware_failed",
            )
            .into_response()
        }
    }
}
