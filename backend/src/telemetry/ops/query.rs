//! 管理端运维错误事件服务。

use thiserror::Error;

use crate::telemetry::{
    ops::store::PgOpsErrorLogStore,
    ops::types::{OpsErrorFilter, OpsErrorLog},
};

/// 管理端运维错误事件错误。
#[derive(Debug, Error)]
pub enum OpsQueryError {
    /// 查询失败。
    #[error("failed to list ops error logs")]
    List,
}

/// 管理端运维错误事件服务。
#[derive(Clone)]
pub struct OpsQueryService {
    store: PgOpsErrorLogStore,
}

impl OpsQueryService {
    /// 构造管理端运维错误事件服务。
    pub fn new(store: PgOpsErrorLogStore) -> Self {
        Self { store }
    }

    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        filter: OpsErrorFilter,
    ) -> Result<crate::infra::json::NumberedPage<OpsErrorLog>, OpsQueryError> {
        self.store
            .list_page(filter, page, page_size)
            .await
            .map_err(|_| OpsQueryError::List)
    }
}
