//! 下游首字节提交事实；传输保活不代表业务事件已交付

use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use crate::diagnostics::TraceContext;

#[derive(Clone, Default)]
pub struct ResponseCommit {
    first: Arc<OnceLock<TransportState>>,
    trace: TraceContext,
}

enum TransportState {
    Committed { at: SystemTime, status: Option<u16> },
    Finished,
}

impl ResponseCommit {
    pub(crate) fn new(trace: TraceContext) -> Self {
        Self {
            trace,
            ..Self::default()
        }
    }

    /// 只记录首次实际交付；已结束且未提交的执行返回 false，不改变业务重放屏障
    pub fn record(&self, status: Option<u16>) -> bool {
        if self
            .first
            .set(TransportState::Committed {
                at: SystemTime::now(),
                status,
            })
            .is_ok()
        {
            self.trace.record(
                "downstream.committed",
                serde_json::json!({"status": status}),
            );
        }
        self.committed_at().is_some()
    }

    /// 冻结交付状态，返回是否仍可发送独立的 HTTP 错误响应
    pub(crate) fn finish(&self) -> bool {
        matches!(
            self.first.get_or_init(|| TransportState::Finished),
            TransportState::Finished
        )
    }

    #[must_use]
    pub fn committed_at(&self) -> Option<SystemTime> {
        match self.first.get() {
            Some(TransportState::Committed { at, .. }) => Some(*at),
            _ => None,
        }
    }

    #[must_use]
    pub fn status_code(&self) -> Option<u16> {
        match self.first.get() {
            Some(TransportState::Committed { status, .. }) => *status,
            _ => None,
        }
    }
}
