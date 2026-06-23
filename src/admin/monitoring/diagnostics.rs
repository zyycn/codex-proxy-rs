//! 诊断数据类型与聚合函数。

use serde::Serialize;

use crate::{
    config::types::AppConfig,
    upstream::accounts::{
        model::{Account, AccountStatus},
        pool::AccountCapacitySummary,
    },
    upstream::fingerprint::Fingerprint,
};

/// 诊断数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsData {
    /// 诊断状态。
    pub status: &'static str,
    /// 运行时包信息。
    pub runtime: RuntimeDiagnostics,
    /// Runtime paths.
    pub paths: PathDiagnostics,
    /// 上游传输配置。
    pub transport: TransportDiagnostics,
    /// 账号状态。
    pub accounts: AccountDiagnostics,
    /// 主要运行设置。
    pub settings: SettingsDiagnostics,
}

/// Runtime path diagnostics.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDiagnostics {
    /// Primary config file.
    pub config: &'static str,
    /// Configured SQLite database URL.
    pub database_url: String,
}

/// 运行时包信息。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDiagnostics {
    /// 包名。
    pub package_name: &'static str,
    /// 包版本。
    pub package_version: &'static str,
}

/// 上游传输配置。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportDiagnostics {
    /// Codex 后端基础 URL。
    pub backend_base_url: String,
    /// TLS 配置。
    pub tls: TlsDiagnostics,
    /// Runtime request fingerprint.
    pub fingerprint: FingerprintDiagnostics,
}

/// TLS 配置。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsDiagnostics {
    /// 是否强制 HTTP/1.1。
    pub force_http11: bool,
}

/// 账号诊断数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDiagnostics {
    /// 账号仓储是否可用。
    pub repository_available: bool,
    /// 账号池摘要。
    pub pool: AccountPoolDiagnostics,
    /// Account-pool capacity.
    pub capacity: AccountCapacityDiagnostics,
}

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

/// Upstream probe diagnostics.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProbeDiagnostics {
    /// Probe target name.
    pub target: &'static str,
    /// Configured backend base URL.
    pub backend_base_url: String,
    /// Full endpoint URL.
    pub endpoint: String,
    /// Whether upstream answered at transport level.
    pub reachable: bool,
    /// HTTP status code, when available.
    pub status_code: Option<u16>,
    /// Authentication outcome inferred from status.
    pub authorization: &'static str,
}

/// 主要运行设置。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDiagnostics {
    /// 默认模型。
    pub default_model: String,
    /// 是否启用刷新。
    pub refresh_enabled: bool,
    /// 账号轮换策略。
    pub rotation_strategy: String,
    /// 是否跳过配额耗尽账号。
    pub quota_skip_exhausted: bool,
    /// 是否启用日志。
    pub logs_enabled: bool,
}

/// 诊断聚合输入。
pub struct DiagnosticsInput<'a> {
    /// 当前配置。
    pub config: &'a AppConfig,
    /// 运行时账号快照。
    pub accounts: &'a [Account],
    /// 账号池容量摘要。
    pub capacity: AccountCapacitySummary,
    /// 当前 fingerprint。
    pub fingerprint: &'a Fingerprint,
}

/// 构造诊断数据。
pub fn diagnostics_data(input: DiagnosticsInput<'_>) -> DiagnosticsData {
    DiagnosticsData {
        status: "ok",
        runtime: RuntimeDiagnostics {
            package_name: env!("CARGO_PKG_NAME"),
            package_version: env!("CARGO_PKG_VERSION"),
        },
        paths: PathDiagnostics {
            config: "config.yaml",
            database_url: input.config.database.url.clone(),
        },
        transport: transport_diagnostics(input.config, input.fingerprint),
        accounts: AccountDiagnostics {
            repository_available: true,
            pool: account_pool_diagnostics(input.accounts),
            capacity: AccountCapacityDiagnostics::from(input.capacity),
        },
        settings: SettingsDiagnostics::from(input.config),
    }
}

fn transport_diagnostics(config: &AppConfig, fingerprint: &Fingerprint) -> TransportDiagnostics {
    TransportDiagnostics {
        backend_base_url: config.api.base_url.clone(),
        tls: TlsDiagnostics {
            force_http11: config.tls.force_http11,
        },
        fingerprint: fingerprint_diagnostics(fingerprint),
    }
}

fn account_pool_diagnostics(accounts: &[Account]) -> AccountPoolDiagnostics {
    let mut summary = AccountPoolDiagnostics {
        total: accounts.len(),
        ..AccountPoolDiagnostics::default()
    };
    for account in accounts {
        match account.status {
            AccountStatus::Active => summary.active += 1,
            AccountStatus::Expired => summary.expired += 1,
            AccountStatus::QuotaExhausted => summary.quota_exhausted += 1,
            AccountStatus::Refreshing => summary.refreshing += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Banned => summary.banned += 1,
        }
    }
    summary
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

impl From<&AppConfig> for SettingsDiagnostics {
    fn from(config: &AppConfig) -> Self {
        Self {
            default_model: config.model.default_model.clone(),
            refresh_enabled: config.auth.refresh_enabled,
            rotation_strategy: config.auth.rotation_strategy.clone(),
            quota_skip_exhausted: config.quota.skip_exhausted,
            logs_enabled: config.logging.enabled,
        }
    }
}
