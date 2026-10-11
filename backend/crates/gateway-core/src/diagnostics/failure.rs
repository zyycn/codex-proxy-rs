//! 控制面与后台操作的受控故障记录，不伪造模型请求或上游发送事实

use async_trait::async_trait;

use crate::{
    account::ProviderAccountId,
    error::{ErrorDetails, OpaqueUpstreamValue, StoreError},
    identity::ProviderKind,
};

#[derive(Debug)]
pub struct OperationalFailure {
    pub occurred_at: std::time::SystemTime,
    pub component: &'static str,
    pub operation: &'static str,
    pub kind: &'static str,
    pub message: String,
    pub correlation_id: Option<String>,
    pub provider_kind: Option<ProviderKind>,
    pub account_id: Option<ProviderAccountId>,
    pub account_snapshot: Option<crate::account::AccountHistorySnapshot>,
    pub upstream_status: Option<u16>,
    pub upstream_code: Option<OpaqueUpstreamValue>,
    pub details: Option<ErrorDetails>,
}

impl OperationalFailure {
    #[must_use]
    pub fn new(
        component: &'static str,
        operation: &'static str,
        kind: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            occurred_at: std::time::SystemTime::now(),
            component,
            operation,
            kind,
            message: message.into(),
            correlation_id: None,
            provider_kind: None,
            account_id: None,
            account_snapshot: None,
            upstream_status: None,
            upstream_code: None,
            details: None,
        }
    }
}

/// 记录遵循既有观测队列的 best-effort 合同，失败不应替换业务结果
#[async_trait]
pub trait OperationalDiagnostics: Send + Sync {
    async fn record_failure(&self, failure: OperationalFailure) -> Result<(), StoreError>;
}
