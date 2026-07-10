//! 管理端运维错误事件服务。

use chrono::Utc;
use thiserror::Error;

use crate::telemetry::{
    ops::store::{OpsErrorFilter, PgOpsErrorLogStore},
    ops::types::OpsErrorLog,
    usage::types::{metadata_i64, metadata_service_tier, metadata_string},
};

/// 管理端运维错误事件错误。
#[derive(Debug, Error)]
pub enum OpsQueryError {
    /// 写入失败。
    #[error("failed to append ops error log")]
    Append,
    /// 查询失败。
    #[error("failed to list ops error logs")]
    List,
    /// 保留期清理失败。
    #[error("failed to trim expired ops error logs")]
    Retention,
}

/// 管理端运维错误事件服务。
#[derive(Clone)]
pub struct OpsQueryService {
    store: PgOpsErrorLogStore,
    capture_body: bool,
}

impl OpsQueryService {
    /// 构造管理端运维错误事件服务。
    pub fn new(store: PgOpsErrorLogStore, capture_body: bool) -> Self {
        Self {
            store,
            capture_body,
        }
    }

    /// 记录运维错误事件。
    pub async fn record(&self, mut event: OpsErrorLog) -> Result<(), OpsQueryError> {
        lift_error_fact_fields(&mut event);
        apply_capture_body_policy(&mut event, self.capture_body);
        self.store
            .append(&event)
            .await
            .map_err(|_| OpsQueryError::Append)?;
        Ok(())
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
        filter: OpsErrorFilter,
    ) -> Result<crate::infra::json::Page<OpsErrorLog>, OpsQueryError> {
        self.store
            .list(filter, cursor, limit)
            .await
            .map_err(|_| OpsQueryError::List)
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

    /// 周期清理超过保留期的错误事实。
    pub async fn trim_to_retention(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<u64, OpsQueryError> {
        self.store
            .trim_to_retention(now)
            .await
            .map_err(|_| OpsQueryError::Retention)
    }
}

fn lift_error_fact_fields(event: &mut OpsErrorLog) {
    event.client_status_code = event
        .client_status_code
        .or_else(|| metadata_i64(&event.metadata, &["clientStatusCode", "client_status_code"]));
    event.upstream_status_code = event
        .upstream_status_code
        .or_else(|| metadata_i64(&event.metadata, &["upstreamStatusCode", "upstreamStatus"]));
    event.transport = event
        .transport
        .take()
        .or_else(|| metadata_string(&event.metadata, &["transport"]));
    event.attempt_index = event
        .attempt_index
        .or_else(|| metadata_i64(&event.metadata, &["attemptIndex", "attempt_index"]));
    event.failure_class = event
        .failure_class
        .take()
        .or_else(|| metadata_string(&event.metadata, &["failureClass", "failure_class"]));
    event.response_id = event
        .response_id
        .take()
        .or_else(|| metadata_string(&event.metadata, &["responseId", "response_id"]));
    event.upstream_request_id = event.upstream_request_id.take().or_else(|| {
        metadata_string(
            &event.metadata,
            &[
                "upstreamRequestId",
                "upstream_request_id",
                "openaiRequestId",
            ],
        )
    });
    event.service_tier = event
        .service_tier
        .take()
        .or_else(|| metadata_service_tier(&event.metadata).map(ToString::to_string));

    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "clientStatusCode",
        "client_status_code",
        "upstreamStatusCode",
        "upstreamStatus",
        "transport",
        "attemptIndex",
        "attempt_index",
        "failureClass",
        "failure_class",
        "responseId",
        "response_id",
        "upstreamRequestId",
        "upstream_request_id",
        "openaiRequestId",
        "serviceTier",
    ] {
        metadata.remove(key);
    }
}

fn apply_capture_body_policy(event: &mut OpsErrorLog, capture_body: bool) {
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
        if capture_body {
            if let Some(serde_json::Value::String(value)) = metadata.get_mut(key) {
                value.truncate(4096);
            }
        } else {
            metadata.remove(key);
        }
    }
}
