//! 诊断数据类型与聚合函数。

use serde::Serialize;

use crate::{upstream::accounts::pool::AccountCapacitySummary, upstream::fingerprint::Fingerprint};

/// 账号池摘要。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPoolDiagnostics {
    /// 总数。
    pub total: usize,
    /// 活跃数。
    pub active: usize,
    /// 过期数。
    pub expired: usize,
    /// 配额耗尽数。
    pub quota_exhausted: usize,
    /// 刷新中数。
    pub refreshing: usize,
    /// 禁用数。
    pub disabled: usize,
    /// 封禁数。
    pub banned: usize,
}

/// Account-pool capacity diagnostics.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCapacityDiagnostics {
    /// Maximum concurrent slots per account.
    pub max_concurrent_per_account: usize,
    /// Total slots available across active accounts.
    pub total_slots: usize,
    /// Currently occupied slots.
    pub used_slots: usize,
    /// Currently available slots.
    pub available_slots: usize,
}

/// Runtime fingerprint summary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintDiagnostics {
    /// Fingerprint source label.
    pub source: &'static str,
    /// Client originator.
    pub originator: String,
    /// App version.
    pub app_version: String,
    /// Build number.
    pub build_number: String,
    /// Platform name.
    pub platform: String,
    /// Architecture name.
    pub arch: String,
    /// Chromium major version.
    pub chromium_version: String,
    /// Expanded user-agent.
    pub user_agent: String,
    /// 指纹最后更新时间。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// 构造 fingerprint 诊断数据。
pub fn fingerprint_diagnostics(fingerprint: &Fingerprint) -> FingerprintDiagnostics {
    FingerprintDiagnostics {
        source: "runtime",
        originator: fingerprint.originator.clone(),
        app_version: fingerprint.app_version.clone(),
        build_number: fingerprint.build_number.clone(),
        platform: fingerprint.platform.clone(),
        arch: fingerprint.arch.clone(),
        chromium_version: fingerprint.chromium_version.clone(),
        user_agent: fingerprint.user_agent(),
        updated_at: fingerprint.updated_at.clone(),
    }
}

impl From<AccountCapacitySummary> for AccountCapacityDiagnostics {
    fn from(summary: AccountCapacitySummary) -> Self {
        Self {
            max_concurrent_per_account: summary.max_concurrent_per_account,
            total_slots: summary.total_slots,
            used_slots: summary.used_slots,
            available_slots: summary.available_slots,
        }
    }
}
