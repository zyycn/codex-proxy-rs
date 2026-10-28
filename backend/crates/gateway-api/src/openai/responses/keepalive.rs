//! 准入成功后覆盖 Responses 首帧等待；已发心跳的 HTTP 错误转为流内失败

use std::future::Future;

use axum::{
    body::{Body, BodyDataStream, Bytes, to_bytes},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use futures::{StreamExt as _, future::BoxFuture, stream};
use gateway_core::engine::execution::ResponseCommit;
use gateway_protocol::openai::sse::{
    DONE_SSE_FRAME, encode_sse_event, response_failed_sse_data_from_error_event,
    response_failed_sse_data_with_id,
};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::openai::middleware::{SSE_KEEPALIVE_FRAME, SSE_KEEPALIVE_INTERVAL};

pub(super) async fn response(
    response: impl Future<Output = Response> + Send + 'static,
    admitted: oneshot::Receiver<ResponseCommit>,
) -> Response {
    response_with_encoder(response, admitted, None).await
}

pub(super) async fn response_with_encoder(
    response: impl Future<Output = Response> + Send + 'static,
    admitted: oneshot::Receiver<ResponseCommit>,
    error_encoder: Option<fn(&Value) -> Bytes>,
) -> Response {
    let mut response = Box::pin(response);
    // 鉴权、模型门禁和准入失败仍保留原 HTTP 状态，不提前建立 SSE
    let commit = tokio::select! {
        biased;
        response = &mut response => return response,
        admitted = admitted => match admitted {
            Ok(commit) => commit,
            Err(_) => return response.await,
        },
    };
    tokio::select! {
        biased;
        response = &mut response => return response,
        () = tokio::time::sleep(SSE_KEEPALIVE_INTERVAL) => {}
    }
    if !commit.record(Some(StatusCode::OK.as_u16())) {
        return response.await;
    }
    let pending = PendingResponse::Waiting(response, error_encoder);
    let stream = stream::once(async { Ok::<_, axum::Error>(SSE_KEEPALIVE_FRAME) })
        .chain(stream::try_unfold(pending, PendingResponse::next));
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

enum PendingResponse {
    Waiting(BoxFuture<'static, Response>, Option<fn(&Value) -> Bytes>),
    Streaming(BodyDataStream),
}

impl PendingResponse {
    async fn next(mut self) -> Result<Option<(Bytes, Self)>, axum::Error> {
        loop {
            match &mut self {
                Self::Waiting(response, error_encoder) => {
                    // 心跳只轮询同一个 future，不取消重建首帧等待或插件调用
                    let response = tokio::select! {
                        biased;
                        response = response => response,
                        () = tokio::time::sleep(SSE_KEEPALIVE_INTERVAL) => return Ok(Some((SSE_KEEPALIVE_FRAME, self))),
                    };
                    let sse = response.status().is_success()
                        && response
                            .headers()
                            .get(header::CONTENT_TYPE)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.split(';').next())
                            .is_some_and(|value| {
                                value.trim().eq_ignore_ascii_case("text/event-stream")
                            });
                    let body = if sse {
                        response.into_body()
                    } else {
                        stream_failure(response, *error_encoder).await
                    };
                    // 正式响应的正文已经带有交付期心跳，后续只转交，不叠加定时器
                    self = Self::Streaming(body.into_data_stream());
                }
                Self::Streaming(body) => {
                    return body
                        .next()
                        .await
                        .transpose()
                        .map(|chunk| chunk.map(|chunk| (chunk, self)));
                }
            }
        }
    }
}

async fn stream_failure(response: Response, error_encoder: Option<fn(&Value) -> Bytes>) -> Body {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let json = serde_json::from_slice::<Value>(&bytes).ok();
    let raw_message = std::str::from_utf8(&bytes)
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    // 已完成客户端错误投影的对象原样保留，包括重试建议和策略错误的扩展字段
    let mut data = json
        .as_ref()
        .and_then(|json| response_failed_sse_data_from_error_event(None, None, json))
        .unwrap_or_else(|| {
            response_failed_sse_data_with_id(
                None,
                "server_error",
                "upstream_error",
                raw_message.unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("Response stream failed")
                }),
            )
        });
    // HTTP 头已提交，按官方 SSE 错误合同保留允许下发的重试建议
    if let Some(retry_after) = retry_after
        && let Some(error) = data["response"]["error"].as_object_mut()
        && let Some(headers) = error
            .entry("headers")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
    {
        headers
            .entry("retry-after")
            .or_insert(Value::String(retry_after));
    }
    let mut event = match error_encoder {
        Some(encode) => encode(&data).to_vec(),
        None => encode_sse_event("response.failed", &data.to_string()).into_bytes(),
    };
    event.extend_from_slice(DONE_SSE_FRAME.as_bytes());
    Body::from(event)
}
