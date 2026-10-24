//! Provider 发送前的完整请求准备；续体只组合值，不执行网络操作

use std::fmt;

use bytes::Bytes;
use futures::FutureExt;

use super::{FrozenMiddlewarePlan, MiddlewareContext, MiddlewareError, MiddlewareHeader};
use crate::error::{ProviderError, ProviderErrorKind};
use crate::upstream::UpstreamSendState;

/// 当前发送所用的传输类型与目标；准备过程中不能切换传输类型
#[derive(Clone, PartialEq, Eq)]
pub enum Target {
    Http {
        method: http::Method,
        uri: http::Uri,
    },
    WebSocket {
        uri: http::Uri,
    },
}

impl fmt::Debug for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { method, .. } => formatter
                .debug_struct("Http")
                .field("method", method)
                .finish_non_exhaustive(),
            Self::WebSocket { .. } => formatter.debug_struct("WebSocket").finish_non_exhaustive(),
        }
    }
}

/// HTTP 正文已完成序列化与内容编码；WebSocket 正文是一条完整文本消息
///
/// WebSocket headers 表示候选 opening，不能改写已经建立的连接握手
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub target: Target,
    pub headers: Vec<MiddlewareHeader>,
    pub body: Bytes,
}

impl Request {
    /// 转换实际发送的 HTTP 头，保留多值与原始字节，不修复插件输出
    pub fn header_map(&self) -> Result<http::HeaderMap, ProviderError> {
        let mut headers = http::HeaderMap::try_with_capacity(self.headers.len())
            .map_err(|_| preparation_error(ProviderErrorKind::InvalidRequest))?;
        for header in &self.headers {
            let name = http::HeaderName::from_bytes(header.name().as_bytes())
                .map_err(|_| preparation_error(ProviderErrorKind::InvalidRequest))?;
            let value = http::HeaderValue::from_bytes(header.value())
                .map_err(|_| preparation_error(ProviderErrorKind::InvalidRequest))?;
            headers
                .try_append(name, value)
                .map_err(|_| preparation_error(ProviderErrorKind::InvalidRequest))?;
        }
        Ok(headers)
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("target", &self.target)
            .field("headers", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

pub type Next = crate::middleware::Next<Request, Request, MiddlewareError>;

/// 保活本次 attempt 的冻结发布代次、归属和资源上下文
#[derive(Clone, Debug)]
pub struct Pipeline {
    plan: FrozenMiddlewarePlan,
    context: MiddlewareContext,
}

impl Pipeline {
    pub(in crate::engine) const fn new(
        plan: FrozenMiddlewarePlan,
        context: MiddlewareContext,
    ) -> Self {
        Self { plan, context }
    }

    /// 完整返回洋葱链最终输出后，Provider 才能提交实际发送
    pub async fn prepare(&self, request: Request) -> Result<Request, ProviderError> {
        if self.context.cancellation().is_cancelled() {
            return Err(preparation_error(ProviderErrorKind::Cancelled));
        }
        if self.context.deadline().is_elapsed() {
            return Err(preparation_error(ProviderErrorKind::Timeout));
        }
        let target_kind = std::mem::discriminant(&request.target);
        let next =
            crate::middleware::compose(Vec::new(), |request| Box::pin(async { Ok(request) }));
        let prepare = self
            .plan
            .handle_outbound(self.context.clone(), request, next)
            .fuse();
        let cancelled = self.context.cancellation().cancelled().fuse();
        let expired = self.context.deadline().wait().fuse();
        futures::pin_mut!(prepare, cancelled, expired);
        let prepared = futures::select_biased! {
            () = cancelled => return Err(preparation_error(ProviderErrorKind::Cancelled)),
            () = expired => return Err(preparation_error(ProviderErrorKind::Timeout)),
            result = prepare => result.map_err(|error| {
                crate::engine::provider::middleware_error(error, UpstreamSendState::NotSent)
                    .with_retry_prohibited()
            })?,
        };
        if self.context.cancellation().is_cancelled() {
            return Err(preparation_error(ProviderErrorKind::Cancelled));
        }
        if self.context.deadline().is_elapsed() {
            return Err(preparation_error(ProviderErrorKind::Timeout));
        }
        if std::mem::discriminant(&prepared.target) != target_kind
            || self
                .context
                .request_body_limit()
                .is_some_and(|limit| prepared.body.len() > limit)
        {
            return Err(preparation_error(ProviderErrorKind::InvalidRequest));
        }
        Ok(prepared)
    }
}

fn preparation_error(kind: ProviderErrorKind) -> ProviderError {
    ProviderError::new(kind, UpstreamSendState::NotSent)
        .with_retry_prohibited()
        .redact_sensitive_context("invalid outbound request preparation")
}
