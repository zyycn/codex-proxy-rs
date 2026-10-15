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
    /// 保留期管理与全量设置保存共用的字段约束
    pub fn validate_values(
        usage_days: u32,
        ops_days: u32,
        audit_days: u32,
    ) -> Result<(), &'static str> {
        for (valid, field) in [
            ((1..=31).contains(&usage_days), "usage_retention_days"),
            (ops_days > 0, "ops_event_retention_days"),
            (audit_days > 0, "audit_retention_days"),
        ] {
            if !valid {
                return Err(field);
            }
        }
        Ok(())
    }

    pub fn try_new(usage_days: u32, ops_days: u32, audit_days: u32) -> Result<Self, AdminError> {
        if Self::validate_values(usage_days, ops_days, audit_days).is_err() {
            return Err(AdminError::invalid(
                "请求保留期必须为 1～31 天，事件与审计保留期必须为正数",
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
