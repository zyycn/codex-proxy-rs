//! 手动同步的外部价目来源；网络和外部文档解析由 Host 实现。

use crate::model::{AdminError, pricing::PricingSyncPreview};
use async_trait::async_trait;

#[async_trait]
pub trait PricingSource: Send + Sync {
    async fn fetch(&self) -> Result<PricingSyncPreview, AdminError>;
}
