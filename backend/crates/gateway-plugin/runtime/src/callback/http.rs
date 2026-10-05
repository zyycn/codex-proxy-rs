//! 插件受管 HTTP 回调的请求解码、执行分派与错误映射

use std::sync::Arc;

use gateway_core::upstream::UpstreamSendState;
use gateway_host::outbound::{HttpClient, HttpError, HttpErrorKind, HttpRequest, NetworkPolicy};
use gateway_plugin_sdk::{ErrorCode, PluginFault, SendState, call::host as wire};
use serde::de::DeserializeOwned;

use super::{CallResources, CallbackScope, HttpStream, denied, invalid};
use crate::RpcReply;

/// 回调的公共网络外层；账号鉴权只装饰请求，发送、读流和回收共用同一终端
pub(super) struct HttpCallbacks<'a> {
    pub(super) client: &'a HttpClient,
    pub(super) network: &'a NetworkPolicy,
    pub(super) scope: &'a Arc<CallbackScope>,
    pub(super) call: &'a CallResources,
    pub(super) maximum_payload: usize,
}

impl HttpCallbacks<'_> {
    pub(super) async fn send(
        &self,
        request: HttpRequest,
        streaming: bool,
        upstream_purpose: Option<gateway_plugin_sdk::call::upstream_adapter::UpstreamPathPurpose>,
    ) -> Result<RpcReply, PluginFault> {
        let Self {
            client,
            network,
            scope,
            call,
            maximum_payload,
        } = *self;
        let timeout = call.timeout()?;
        let attempt = scope.start_upstream(upstream_purpose);
        let response = client
            .open(request, scope.proxy.as_ref(), network, timeout)
            .await;
        attempt.finish(
            response
                .as_ref()
                .map_or_else(|error| error.send_state, |_| UpstreamSendState::Sent),
        );
        let response = response.map_err(http_error)?;
        let mut body = response.body;
        let headers = response
            .headers
            .into_iter()
            .map(|(name, value)| String::from_utf8(value).map(|value| (name, value)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| sent_fault(ErrorCode::Fault, "HTTP header cannot be represented"))?;
        let mut result = wire::HttpResponse {
            status: response.status,
            headers,
            stream: None,
        };
        let mut payload = Vec::new();
        if streaming {
            let id = uuid::Uuid::new_v4().to_string();
            let mut state = call
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                return Err(denied());
            }
            if state.streams.len() >= 16 {
                return Err(sent_fault(
                    ErrorCode::Capacity,
                    "HTTP stream capacity exhausted",
                ));
            }
            state.streams.insert(
                id.clone(),
                Arc::new(HttpStream {
                    body: tokio::sync::Mutex::new(Some(body)),
                    closed: tokio::sync::watch::channel(false).0,
                }),
            );
            result.stream = Some(id);
        } else {
            while let Some(chunk) = body.read(64 * 1024).await.map_err(http_error)? {
                if payload.len() + chunk.len() > maximum_payload {
                    return Err(sent_fault(
                        ErrorCode::Capacity,
                        "HTTP body requires streaming",
                    ));
                }
                payload.extend_from_slice(&chunk);
            }
        }
        Ok(RpcReply {
            result: serde_json::to_value(result).map_err(|_| invalid())?,
            payload,
        })
    }

    pub(super) async fn dispatch(
        &self,
        method: &str,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcReply, PluginFault> {
        let call = self.call;
        match method {
            "host.http.do" | "host.http.do_stream" => {
                let request: wire::HttpRequest = decode(params)?;
                self.send(
                    HttpRequest {
                        method: request.method,
                        url: request.url,
                        headers: request
                            .headers
                            .into_iter()
                            .map(|(name, value)| (name, value.into_bytes()))
                            .collect(),
                        body: payload,
                    },
                    method == "host.http.do_stream",
                    None,
                )
                .await
            }
            "host.http.stream_read" => {
                if !payload.is_empty() {
                    return Err(invalid());
                }
                let request: wire::StreamRead = decode(params)?;
                if request.maximum_bytes == 0 || request.maximum_bytes > 64 * 1024 {
                    return Err(invalid());
                }
                let stream = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .streams
                    .get(&request.stream)
                    .cloned()
                    .ok_or_else(denied)?;
                let mut body = stream.body.try_lock().map_err(|_| {
                    PluginFault::new(ErrorCode::Capacity, "HTTP stream already has a reader")
                })?;
                let mut closed = stream.closed.subscribe();
                let result = tokio::select! {
                    biased;
                    _ = closed.wait_for(|closed| *closed) => Err(denied()),
                    result = body.as_mut().ok_or_else(denied)?.read(request.maximum_bytes as usize) => result.map_err(http_error),
                };
                if !matches!(&result, Ok(Some(_))) {
                    body.take();
                    stream.closed.send_replace(true);
                    call.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .streams
                        .remove(&request.stream);
                }
                let chunk = result?;
                Ok(RpcReply {
                    result: serde_json::json!({"eof":chunk.is_none()}),
                    payload: chunk.map_or_else(Vec::new, |chunk| chunk.to_vec()),
                })
            }
            "host.http.stream_close" => {
                if !payload.is_empty() {
                    return Err(invalid());
                }
                let request: wire::StreamClose = decode(params)?;
                let stream = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .streams
                    .remove(&request.stream)
                    .ok_or_else(denied)?;
                stream.close();
                Ok(RpcReply {
                    result: serde_json::json!({}),
                    payload: vec![],
                })
            }
            _ => Err(denied()),
        }
    }
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, PluginFault> {
    serde_json::from_value(value).map_err(|_| invalid())
}

fn sent_fault(code: ErrorCode, message: &'static str) -> PluginFault {
    let mut error = PluginFault::new(code, message);
    error.send_state = SendState::Sent;
    error
}

pub(super) fn http_error(error: HttpError) -> PluginFault {
    tracing::warn!(
        reason = error.reason(),
        kind = ?error.kind(),
        send_state = ?error.send_state,
        "插件受管 HTTP 请求失败"
    );
    let (code, message) = match error.kind() {
        HttpErrorKind::InvalidRequest => (ErrorCode::InvalidInput, "网络请求参数无效"),
        HttpErrorKind::AddressDenied => (
            ErrorCode::PermissionDenied,
            "目标地址被安全策略拦截，请检查宿主 DNS 或代理设置",
        ),
        HttpErrorKind::Dns => (ErrorCode::Upstream, "域名解析失败，请检查宿主 DNS"),
        HttpErrorKind::Timeout => (ErrorCode::Timeout, "网络请求超时，请稍后重试"),
        HttpErrorKind::Transport => (ErrorCode::Upstream, "无法连接目标网站，请检查网络或代理"),
        HttpErrorKind::Capacity => (ErrorCode::Capacity, "网络请求繁忙，请稍后重试"),
        HttpErrorKind::Response => (ErrorCode::Upstream, "目标网站响应读取失败"),
    };
    let mut fault = PluginFault::new(code, message);
    fault.send_state = match error.send_state {
        UpstreamSendState::NotSent => SendState::NotSent,
        UpstreamSendState::Sent => SendState::Sent,
        UpstreamSendState::Ambiguous => SendState::Ambiguous,
    };
    fault
}
