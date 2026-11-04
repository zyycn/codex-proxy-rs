//! 将用量读取绑定到父调用，Admin 拥有查询语义，Runtime 管理句柄和回收

use super::{CallResources, denied, invalid};
use crate::RpcReply;
use gateway_admin::{
    UsageDataService,
    model::{AdminError, usage_data::*},
    ports::usage_data::UsageReadSession,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_plugin_sdk::{ErrorCode, PluginFault, call::services::usage as wire};
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct UsagePorts(OnceLock<Weak<dyn UsageDataService>>);

pub(super) struct UsageReader {
    session: Mutex<Box<dyn UsageReadSession>>,
    cancellation: CancellationToken,
}

impl UsageReader {
    pub(super) fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl UsagePorts {
    pub(crate) fn bind(&self, service: &Arc<dyn UsageDataService>) -> Result<(), AdminError> {
        self.0
            .set(Arc::downgrade(service))
            .map_err(|_| AdminError::conflict("用量查询端口已经绑定"))
    }

    pub(super) async fn call(
        &self,
        call: &CallResources,
        method: &str,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if params != serde_json::json!({}) || payload.len() > USAGE_READ_MAX_BATCH_BYTES {
            return Err(invalid());
        }
        match method {
            wire::RECORDS_SCAN | wire::STATISTICS_QUERY => {
                let query = if method == wire::RECORDS_SCAN {
                    UsageReadQuery::Records(serde_json::from_slice(payload).map_err(|_| invalid())?)
                } else {
                    UsageReadQuery::Statistics(
                        serde_json::from_slice(payload).map_err(|_| invalid())?,
                    )
                };
                let service = self
                    .0
                    .get()
                    .and_then(Weak::upgrade)
                    .ok_or_else(|| PluginFault::new(ErrorCode::Fault, "用量查询端口不可用"))?;
                let cancellation = call.cancellation.child_token();
                let mut session = service
                    .open_read(query, cancellation.clone())
                    .await
                    .map_err(fault)?;
                let batch = session.next_batch().await.map_err(fault)?;
                let reader = (!batch.complete).then(|| uuid::Uuid::new_v4().to_string());
                let reply = super::admin::encode(&UsageReadOpened {
                    reader: reader.clone(),
                    batch,
                })?;
                if let Some(reader) = reader {
                    let mut state = call
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.closed {
                        return Err(denied());
                    }
                    state.usage_readers.retain(|_, reader| {
                        !reader
                            .session
                            .try_lock()
                            .is_ok_and(|session| session.is_finished())
                    });
                    state.usage_readers.insert(
                        reader,
                        Arc::new(UsageReader {
                            session: Mutex::new(session),
                            cancellation,
                        }),
                    );
                }
                Ok(reply)
            }
            wire::READ_NEXT | wire::READ_CLOSE => {
                let request: UsageReadHandle =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let reader = {
                    let mut state = call
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if method == wire::READ_CLOSE {
                        state.usage_readers.remove(&request.reader)
                    } else {
                        state.usage_readers.get(&request.reader).cloned()
                    }
                }
                .ok_or_else(|| {
                    fault(UsageDataError::new(
                        UsageErrorKind::NotFound,
                        "用量读取句柄无效或已结束",
                    ))
                })?;
                if method == wire::READ_CLOSE {
                    reader.cancel();
                    reader.session.lock().await.close().await;
                    return super::admin::encode(&());
                }
                let mut session = reader.session.try_lock().map_err(|_| {
                    PluginFault::new(ErrorCode::Conflict, "同一用量读取只允许一个消费者")
                })?;
                let result = session.next_batch().await;
                if result.as_ref().map_or(true, |batch| batch.complete) {
                    call.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .usage_readers
                        .remove(&request.reader);
                    session.close().await;
                }
                super::admin::encode(&result.map_err(fault)?)
            }
            _ => Err(invalid()),
        }
    }
}

fn fault(error: UsageDataError) -> PluginFault {
    let code = match error.kind {
        UsageErrorKind::InvalidInput => ErrorCode::InvalidInput,
        UsageErrorKind::NotFound => ErrorCode::Conflict,
        UsageErrorKind::Conflict => ErrorCode::Conflict,
        UsageErrorKind::CapacityExceeded => ErrorCode::Capacity,
        UsageErrorKind::Unavailable => ErrorCode::Fault,
        UsageErrorKind::Cancelled => ErrorCode::Cancelled,
        UsageErrorKind::Timeout => ErrorCode::Timeout,
    };
    let mut fault = PluginFault::new(code, error.message.clone());
    fault.details = serde_json::to_value(error).ok();
    fault
}
