//! 上游 Codex 网关端口。

use async_trait::async_trait;
use thiserror::Error;

use crate::models::model::BackendModelEntry;

/// 拉取上游模型目录时的请求上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexModelCatalogRequest<'a> {
    /// 当前账号访问令牌。
    pub access_token: &'a str,
    /// 上游账号 ID。
    pub account_id: Option<&'a str>,
    /// 请求 ID。
    pub request_id: &'a str,
    /// Codex installation id。
    pub installation_id: Option<&'a str>,
    /// 订阅计划类型。
    pub plan_type: &'a str,
}

/// 上游模型目录客户端错误。
#[derive(Debug, Error)]
pub enum CodexModelCatalogClientError {
    /// 上游请求失败。
    #[error("model catalog request failed: {message}")]
    RequestFailed {
        /// 错误说明。
        message: String,
    },
}

/// 上游模型目录客户端。
#[async_trait]
pub trait CodexModelCatalogClient: Send + Sync + 'static {
    /// 读取当前账号可见的上游模型目录。
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, CodexModelCatalogClientError>;
}
