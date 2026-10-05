//! 历史数据保留规则；存储实现只接受已校验的窗口

use super::AdminError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTarget {
    ModelRequests,
    OpsEvents,
    AdminAuditEvents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    usage_days: u32,
    ops_days: u32,
    audit_days: u32,
}

impl RetentionPolicy {
    pub fn try_new(usage_days: u32, ops_days: u32, audit_days: u32) -> Result<Self, AdminError> {
        if usage_days < 31 || ops_days == 0 || audit_days == 0 {
            return Err(AdminError::invalid(
                "请求保留期至少 31 天，事件与审计保留期必须为正数",
            ));
        }
        Ok(Self {
            usage_days,
            ops_days,
            audit_days,
        })
    }

    #[must_use]
    pub const fn days(self, target: RetentionTarget) -> u32 {
        match target {
            RetentionTarget::ModelRequests => self.usage_days,
            RetentionTarget::OpsEvents => self.ops_days,
            RetentionTarget::AdminAuditEvents => self.audit_days,
        }
    }
}
