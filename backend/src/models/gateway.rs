//! 模型目录用例拥有的上游端口。

use async_trait::async_trait;

use super::types::BackendModelEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCatalogRequest<'a> {
    pub access_token: &'a str,
    pub account_id: Option<&'a str>,
    pub request_id: &'a str,
    pub installation_id: Option<&'a str>,
    pub plan_type: &'a str,
}

#[derive(Debug, thiserror::Error)]
#[error("model catalog request failed: {message}")]
pub struct ModelCatalogSourceError {
    message: String,
}

impl ModelCatalogSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait ModelCatalogSource: Send + Sync + 'static {
    async fn fetch_models(
        &self,
        request: &ModelCatalogRequest<'_>,
    ) -> Result<Vec<BackendModelEntry>, ModelCatalogSourceError>;
}
