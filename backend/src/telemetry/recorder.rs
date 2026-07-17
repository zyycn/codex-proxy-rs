//! 已完成遥测事实的持久化入口。

use serde_json::Value;

use crate::telemetry::{
    ops::store::{PgOpsErrorLogStore, PgOpsErrorLogStoreError},
    ops::types::OpsErrorLog,
    usage::{
        store::{PgUsageRecordStore, PgUsageRecordStoreError},
        types::UsageRecord,
    },
};

const MAX_CAPTURED_BODY_BYTES: usize = 4 * 1024;

#[derive(Clone)]
pub struct Recorder {
    usage_records: PgUsageRecordStore,
    ops_errors: PgOpsErrorLogStore,
    usage_enabled: bool,
    capture_body: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    #[error("invalid success usage fact")]
    InvalidUsageFact,
    #[error(transparent)]
    Usage(#[from] PgUsageRecordStoreError),
    #[error(transparent)]
    Ops(#[from] PgOpsErrorLogStoreError),
}

impl Recorder {
    pub fn new(
        usage_records: PgUsageRecordStore,
        ops_errors: PgOpsErrorLogStore,
        usage_enabled: bool,
        capture_body: bool,
    ) -> Self {
        Self {
            usage_records,
            ops_errors,
            usage_enabled,
            capture_body,
        }
    }

    pub async fn record_usage(&self, mut event: UsageRecord) -> Result<(), RecorderError> {
        if !self.usage_enabled {
            return Ok(());
        }
        if !is_usage_fact(&event) {
            tracing::error!(
                usage_record_id = %event.id,
                request_id = event.request_id.as_deref().unwrap_or(""),
                account_id = event.account_id,
                model = event.model,
                status_code = event.status_code,
                "Rejected invalid success usage fact"
            );
            return Err(RecorderError::InvalidUsageFact);
        }
        apply_capture_body_policy(&mut event.metadata, self.capture_body);
        self.usage_records.append(&event).await?;
        Ok(())
    }

    pub async fn record_error(&self, mut event: OpsErrorLog) -> Result<(), RecorderError> {
        apply_capture_body_policy(&mut event.metadata, self.capture_body);
        self.ops_errors.append(&event).await?;
        Ok(())
    }

    pub(crate) fn captures_body(&self) -> bool {
        self.capture_body
    }
}

fn is_usage_fact(event: &UsageRecord) -> bool {
    (200..=399).contains(&event.status_code)
        && !event.provider.trim().is_empty()
        && !event.account_id.trim().is_empty()
        && !event.model.trim().is_empty()
}

fn apply_capture_body_policy(metadata: &mut Value, capture_body: bool) {
    if capture_body {
        limit_body_fields(metadata);
    } else {
        remove_body_fields(metadata);
    }
}

fn limit_body_fields(metadata: &mut Value) {
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };
    for key in body_fields() {
        let Some(value) = metadata.get_mut(key) else {
            continue;
        };
        match value {
            Value::String(value) => truncate_utf8(value, MAX_CAPTURED_BODY_BYTES),
            value => {
                let Ok(mut encoded) = serde_json::to_string(value) else {
                    continue;
                };
                if encoded.len() > MAX_CAPTURED_BODY_BYTES {
                    truncate_utf8(&mut encoded, MAX_CAPTURED_BODY_BYTES);
                    *value = Value::String(encoded);
                }
            }
        }
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn remove_body_fields(metadata: &mut Value) {
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };
    for key in body_fields() {
        metadata.remove(key);
    }
}

fn body_fields() -> [&'static str; 5] {
    [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ]
}
