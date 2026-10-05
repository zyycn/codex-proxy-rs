//! HTTP 正文句柄封装；普通出站和已选账号出站共用惰性读取与提前关闭语义

use serde::{Serialize, de::DeserializeOwned};

use super::{HostClient, HostReply, SessionError, read::PendingRead};
use crate::{
    ErrorCode, PluginFault,
    call::{host as wire, upstream_adapter::UpstreamHttpRequest},
};

/// 响应头与惰性正文；不包含宿主的内部流句柄
pub struct HostHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: HostHttpBody,
}

/// 父调用内的单消费者正文；不后台预读，父调用结束或取消时宿主兜底回收
pub struct HostHttpBody {
    host: HostClient,
    stream: Option<String>,
    upstream: bool,
    read: PendingRead<Result<HostReply, SessionError>>,
}

impl HostClient {
    /// 使用受管 HTTP 发送请求；账号上游使用 [`Self::upstream_http`]
    ///
    /// # Errors
    /// 期限、请求或网络无效时失败
    pub async fn http(
        &self,
        request: wire::HttpRequest,
        body: Vec<u8>,
    ) -> Result<HostHttpResponse, PluginFault> {
        self.open_http("host.http.do_stream", request, body, false)
            .await
    }

    /// 由宿主附加已选账号的认证与代理，返回统一的惰性正文
    ///
    /// # Errors
    /// 路径、期限、网络或宿主响应无效时失败
    pub async fn upstream_http(
        &self,
        request: UpstreamHttpRequest,
        body: Vec<u8>,
    ) -> Result<HostHttpResponse, PluginFault> {
        self.open_http("host.upstream.http.do_stream", request, body, true)
            .await
    }

    async fn open_http<T: Serialize>(
        &self,
        method: &str,
        request: T,
        body: Vec<u8>,
        upstream: bool,
    ) -> Result<HostHttpResponse, PluginFault> {
        let (response, payload): (wire::HttpResponse, _) =
            callback(self, method, request, body).await?;
        if !payload.is_empty() || response.stream.as_ref().is_none_or(String::is_empty) {
            return Err(invalid());
        }
        Ok(HostHttpResponse {
            status: response.status,
            headers: response.headers,
            body: HostHttpBody {
                host: self.clone(),
                stream: response.stream,
                upstream,
                read: PendingRead::default(),
            },
        })
    }
}

impl HostHttpBody {
    /// 按需读取至多 64 KiB，EOF 后再次读取不再调用宿主
    /// 取消一次等待后，再次调用会继续同一次读取，不丢弃分块
    ///
    /// # Errors
    /// 父调用结束、流无效或网络失败时返回错误
    pub async fn read(&mut self) -> Result<Option<Vec<u8>>, PluginFault> {
        let Some(stream) = &self.stream else {
            return Ok(None);
        };
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Read {
            eof: bool,
        }
        let maximum_bytes = u32::try_from(self.host.maximum_stream_chunk_bytes().min(64 * 1024))
            .map_err(|_| invalid())?;
        let reply = self
            .read
            .run(|| {
                let host = self.host.clone();
                let params = serde_json::json!({"stream": stream, "maximum_bytes": maximum_bytes});
                let method = if self.upstream {
                    "host.upstream.http.stream_read"
                } else {
                    "host.http.stream_read"
                };
                async move { host.call(method, params, vec![]).await }
            })
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let result: Read = serde_json::from_value(reply.result).map_err(|_| invalid())?;
        let payload = reply.payload;
        if payload.len() > maximum_bytes as usize || (result.eof && !payload.is_empty()) {
            let _ = self.close().await;
            return Err(invalid());
        }
        if result.eof {
            self.stream = None;
            Ok(None)
        } else {
            Ok(Some(payload))
        }
    }

    /// 收集有限正文；超出调用方预算或读取失败时提前关闭，不把流无限缓冲到内存
    ///
    /// # Errors
    /// 读取失败或超出 `maximum_bytes` 时返回错误
    pub async fn collect(mut self, maximum_bytes: usize) -> Result<Vec<u8>, PluginFault> {
        let mut body = Vec::new();
        loop {
            match self.read().await {
                Ok(Some(chunk)) if chunk.len() <= maximum_bytes.saturating_sub(body.len()) => {
                    body.extend(chunk)
                }
                Ok(None) => return Ok(body),
                result => {
                    let _ = self.close().await;
                    return Err(result.err().unwrap_or_else(|| {
                        PluginFault::new(
                            ErrorCode::Capacity,
                            "HTTP body exceeds the collection limit",
                        )
                    }));
                }
            }
        }
    }

    /// 提前释放正文，重复关闭或 EOF 后关闭不再调用宿主
    ///
    /// # Errors
    /// 父调用已结束或宿主拒绝释放时失败
    pub async fn close(&mut self) -> Result<(), PluginFault> {
        let Some(stream) = &self.stream else {
            return Ok(());
        };
        empty_callback(
            &self.host,
            if self.upstream {
                "host.upstream.http.stream_close"
            } else {
                "host.http.stream_close"
            },
            wire::StreamClose {
                stream: stream.clone(),
            },
            vec![],
        )
        .await?;
        self.stream = None;
        self.read.clear();
        Ok(())
    }
}

pub(super) async fn callback<T: Serialize, R: DeserializeOwned>(
    host: &HostClient,
    method: &str,
    request: T,
    body: Vec<u8>,
) -> Result<(R, Vec<u8>), PluginFault> {
    let reply = host
        .call(
            method,
            serde_json::to_value(request).map_err(|_| invalid())?,
            body,
        )
        .await
        .map_err(SessionError::into_plugin_fault)?;
    Ok((
        serde_json::from_value(reply.result).map_err(|_| invalid())?,
        reply.payload,
    ))
}

pub(super) async fn empty_callback<T: Serialize>(
    host: &HostClient,
    method: &str,
    request: T,
    body: Vec<u8>,
) -> Result<(), PluginFault> {
    let (metadata, payload): (serde_json::Value, _) = callback(host, method, request, body).await?;
    if metadata != serde_json::json!({}) || !payload.is_empty() {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn invalid() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "invalid network callback payload")
}
