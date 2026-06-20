use super::*;

/// 管理端日志服务。
#[derive(Clone)]
pub struct AdminLogService {
    store: SqliteEventLogStore,
    settings: Arc<RwLock<AdminLogSettings>>,
}

#[derive(Debug, Clone, Copy)]
struct AdminLogSettings {
    enabled: bool,
    capacity: u32,
    capture_body: bool,
}

impl AdminLogService {
    /// 构造管理端日志服务。
    pub fn new(
        store: SqliteEventLogStore,
        enabled: bool,
        capacity: u32,
        capture_body: bool,
    ) -> Self {
        Self {
            store,
            settings: Arc::new(RwLock::new(AdminLogSettings {
                enabled,
                capacity,
                capture_body,
            })),
        }
    }

    /// 分页查询日志。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
        filter: AdminLogFilter,
    ) -> Result<Page<EventLog>, AdminLogError> {
        self.store
            .list(filter.into(), cursor, limit)
            .await
            .map_err(|_| AdminLogError::List)
    }

    /// 按 ID 读取日志。
    pub async fn get(&self, id: &str) -> Result<Option<EventLog>, AdminLogError> {
        self.store.get(id).await.map_err(|_| AdminLogError::Get)
    }

    /// 读取日志状态。
    pub async fn state(&self) -> Result<AdminLogState, AdminLogError> {
        let settings = *self.settings.read().await;
        Ok(AdminLogState {
            enabled: settings.enabled,
            capacity: settings.capacity,
            capture_body: settings.capture_body,
            stored_count: self.store.count().await.map_err(|_| AdminLogError::Count)?,
        })
    }

    /// 更新日志状态。
    pub async fn update_state(
        &self,
        update: AdminLogStateUpdate,
    ) -> Result<AdminLogState, AdminLogError> {
        if matches!(update.capacity, Some(0)) {
            return Err(AdminLogError::InvalidCapacity);
        }

        let trim_capacity = {
            let mut settings = self.settings.write().await;
            if let Some(enabled) = update.enabled {
                settings.enabled = enabled;
            }
            if let Some(capacity) = update.capacity {
                settings.capacity = capacity;
            }
            if let Some(capture_body) = update.capture_body {
                settings.capture_body = capture_body;
            }
            update.capacity
        };

        if let Some(capacity) = trim_capacity {
            self.store
                .trim_to_capacity(capacity)
                .await
                .map_err(|_| AdminLogError::Trim)?;
        }

        self.state().await
    }

    /// 清空日志。
    pub async fn clear(&self) -> Result<AdminClearLogs, AdminLogError> {
        self.store
            .clear()
            .await
            .map(|cleared| AdminClearLogs { cleared })
            .map_err(|_| AdminLogError::Clear)
    }

    pub(super) async fn record(&self, mut event: EventLog) -> Result<(), AdminLogError> {
        let settings = *self.settings.read().await;
        let policy = EventLogService::new(settings.enabled);
        if !policy.should_record(&event) {
            return Ok(());
        }
        apply_capture_body_policy(&mut event, settings.capture_body);
        self.store
            .append(&event)
            .await
            .map_err(|_| AdminLogError::Append)?;
        self.store
            .trim_to_capacity(settings.capacity)
            .await
            .map_err(|_| AdminLogError::Trim)?;
        Ok(())
    }
}

fn apply_capture_body_policy(event: &mut EventLog, capture_body: bool) {
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

/// 日志查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<EventLevel>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
}

/// 日志状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogState {
    /// 是否启用。
    pub enabled: bool,
    /// 内存容量。
    pub capacity: u32,
    /// 是否捕获请求体。
    pub capture_body: bool,
    /// 已存储数量。
    pub stored_count: u64,
}

/// 日志状态更新。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogStateUpdate {
    /// 是否启用。
    pub enabled: Option<bool>,
    /// 日志容量。
    pub capacity: Option<u32>,
    /// 是否捕获请求体。
    pub capture_body: Option<bool>,
}

/// 清空日志结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminClearLogs {
    /// 清理数量。
    pub cleared: u64,
}

/// 管理端日志错误。
#[derive(Debug, Error)]
pub enum AdminLogError {
    /// 列表失败。
    #[error("failed to list event logs")]
    List,
    /// 读取失败。
    #[error("failed to get event log")]
    Get,
    /// 计数失败。
    #[error("failed to count event logs")]
    Count,
    /// 清空失败。
    #[error("failed to clear event logs")]
    Clear,
    /// 写入失败。
    #[error("failed to append event log")]
    Append,
    /// 裁剪失败。
    #[error("failed to trim event logs")]
    Trim,
    /// 日志容量非法。
    #[error("log capacity must be greater than zero")]
    InvalidCapacity,
}

impl From<AdminLogFilter> for EventLogFilter {
    fn from(filter: AdminLogFilter) -> Self {
        Self {
            kind: filter.kind,
            level: filter.level,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            transport: filter.transport,
            attempt_index: filter.attempt_index,
            upstream_status_code: filter.upstream_status_code,
            failure_class: filter.failure_class,
            response_id: filter.response_id,
            upstream_request_id: filter.upstream_request_id,
            search: filter.search,
        }
    }
}
