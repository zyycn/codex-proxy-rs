//! 管理端运维错误事件服务。

use chrono::Utc;
use thiserror::Error;

use crate::admin::monitoring::{
    ops_error_model::OpsErrorLog, ops_error_store::SqliteOpsErrorLogStore,
};

/// 管理端运维错误事件错误。
#[derive(Debug, Error)]
pub enum AdminOpsErrorLogError {
    /// 写入失败。
    #[error("failed to append ops error log")]
    Append,
    /// 保留期清理失败。
    #[error("failed to trim expired ops error logs")]
    Retention,
}

/// 管理端运维错误事件服务。
#[derive(Clone)]
pub struct AdminOpsErrorLogService {
    store: SqliteOpsErrorLogStore,
    capture_body: bool,
}

impl AdminOpsErrorLogService {
    /// 构造管理端运维错误事件服务。
    pub fn new(store: SqliteOpsErrorLogStore, capture_body: bool) -> Self {
        Self {
            store,
            capture_body,
        }
    }

    /// 记录运维错误事件。
    pub async fn record(&self, mut event: OpsErrorLog) -> Result<(), AdminOpsErrorLogError> {
        apply_capture_body_policy(&mut event, self.capture_body);
        self.store
            .append(&event)
            .await
            .map_err(|_| AdminOpsErrorLogError::Append)?;
        self.store
            .trim_to_retention(Utc::now())
            .await
            .map_err(|_| AdminOpsErrorLogError::Retention)?;
        Ok(())
    }
}

fn apply_capture_body_policy(event: &mut OpsErrorLog, capture_body: bool) {
    if capture_body {
        return;
    }
    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ] {
        metadata.remove(key);
    }
}
