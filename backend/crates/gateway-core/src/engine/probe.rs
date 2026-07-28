//! 管理端账号连通性测试的真实执行链端口。

use std::fmt;

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::error::{ClientVisibleUpstreamResponse, GatewayError, GatewayErrorKind};
use crate::routing::{ProviderKind, UpstreamModelId};
use crate::{engine::credential::ProviderAccountId, operation::Operation};

#[derive(Debug, Clone, PartialEq)]
pub struct AccountProbeRequest {
    pub account_id: ProviderAccountId,
    pub provider_kind: ProviderKind,
    pub upstream_model: UpstreamModelId,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProbeResult {
    pub text: Vec<String>,
}

/// 仅供当前管理端连接测试展示的原始上游失败响应。
///
/// 正文不进入 `Debug`、日志或持久化；Core 只在探测终态从原始 Provider 错误显式复制。
#[derive(PartialEq, Eq)]
pub struct AccountProbeUpstreamResponse {
    status: u16,
    content_type: Option<Vec<u8>>,
    body: Bytes,
}

impl AccountProbeUpstreamResponse {
    pub(crate) fn from_client_response(response: &ClientVisibleUpstreamResponse) -> Self {
        Self {
            status: response.status(),
            content_type: response.content_type().map(<[u8]>::to_vec),
            body: response.body().clone(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_deref()
    }

    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }
}

impl fmt::Debug for AccountProbeUpstreamResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountProbeUpstreamResponse")
            .field("status", &self.status)
            .field(
                "content_type",
                &self.content_type.as_ref().map(|_| "<present>"),
            )
            .field("body", &"<redacted>")
            .finish()
    }
}

/// 管理端连接测试的终态错误。
///
/// `gateway` 保留稳定分类，`upstream_response` 只面向本次认证管理请求展示源响应。
#[derive(Debug)]
pub struct AccountProbeError {
    gateway: GatewayError,
    upstream_response: Option<AccountProbeUpstreamResponse>,
}

impl AccountProbeError {
    #[must_use]
    pub const fn new(
        gateway: GatewayError,
        upstream_response: Option<AccountProbeUpstreamResponse>,
    ) -> Self {
        Self {
            gateway,
            upstream_response,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> GatewayErrorKind {
        self.gateway.kind()
    }

    #[must_use]
    pub fn client_message(&self) -> &str {
        self.gateway.client_message()
    }

    #[must_use]
    pub fn client_error_type(&self) -> Option<&str> {
        self.gateway.client_error_type()
    }

    #[must_use]
    pub fn client_error_code(&self) -> Option<&str> {
        self.gateway.client_error_code()
    }

    #[must_use]
    pub const fn upstream_response(&self) -> Option<&AccountProbeUpstreamResponse> {
        self.upstream_response.as_ref()
    }
}

impl From<GatewayError> for AccountProbeError {
    fn from(gateway: GatewayError) -> Self {
        Self::new(gateway, None)
    }
}

pub trait AccountProbe: Send + Sync {
    fn probe(
        &self,
        request: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>>;
}
