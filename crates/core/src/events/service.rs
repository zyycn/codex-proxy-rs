//! 事件日志策略服务。

use crate::events::model::{EventLevel, EventLog};

/// 事件日志服务。
#[derive(Debug, Clone)]
pub struct EventLogService {
    enabled: bool,
}

impl EventLogService {
    /// 构造事件日志服务。
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// 判断事件是否应该记录。
    pub fn should_record(&self, event: &EventLog) -> bool {
        self.enabled || event.level == EventLevel::Error
    }
}
