//! 请求调度模块（账号选择、回退、恢复、会话亲和性）。

pub mod chat;
pub mod fallback;
pub mod implicit_resume;
pub mod reasoning_replay;
pub mod recovery;
pub mod responses;
pub mod session_affinity;

use async_trait::async_trait;
use thiserror::Error;

use crate::codex::models::BackendModelEntry;

// ====================================================================
// 上游模型目录端口
// ====================================================================

/// 拉取上游模型目录时的请求上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexModelCatalogRequest<'a> {
    pub access_token: &'a str,
    pub account_id: Option<&'a str>,
    pub request_id: &'a str,
    pub installation_id: Option<&'a str>,
    pub plan_type: &'a str,
}

/// 上游模型目录客户端错误。
#[derive(Debug, Error)]
pub enum CodexModelCatalogClientError {
    #[error("model catalog request failed: {message}")]
    RequestFailed { message: String },
}

/// 上游模型目录客户端。
#[async_trait]
pub trait CodexModelCatalogClient: Send + Sync + 'static {
    async fn fetch_models(
        &self,
        request: &CodexModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, CodexModelCatalogClientError>;
}
