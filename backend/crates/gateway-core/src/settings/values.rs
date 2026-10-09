//! 请求可覆盖的运行设置值，以及共享配置的不可变构造接口

use std::{collections::BTreeMap, sync::Arc};

use crate::identity::ProviderKind;

/// 请求可覆盖的运行设置事实；编译产物不能反向改写本值
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsValues {
    pub(crate) pricing: Arc<crate::metering::PricingOverrides>,
    pub(crate) request_profiles: Arc<BTreeMap<ProviderKind, crate::account::OpaqueProviderData>>,
    pub(crate) codex_privacy_policy: super::privacy::CodexPrivacyPolicy,
    pub(crate) request_location_enabled: bool,
    pub(crate) request_location: crate::account::RequestLocation,
    pub(crate) max_concurrent_per_account: u32,
    pub(crate) max_waiting_per_key: u32,
    pub(crate) max_waiting_per_account: u32,
    pub(crate) concurrency_wait_timeout_seconds: u32,
    pub(crate) openai_guardian_reserved_concurrency: u32,
    pub(crate) openai_account_affinity: crate::account::AccountAffinity,
    pub(crate) max_account_rotations: u32,
    pub(crate) openai_session_affinity_ttl_hours: u32,
    pub(crate) responses_max_decompressed_body_bytes: u64,
    pub(crate) request_interval_ms: u64,
    pub(crate) smart_scheduling: crate::account::SmartSchedulingConfig,
    pub(crate) rotation_strategy: String,
    pub(crate) model_mappings: Arc<BTreeMap<String, String>>,
    pub(crate) min_codex_desktop_version: Option<String>,
    pub(crate) min_codex_cli_version: Option<String>,
}

impl SettingsValues {
    #[must_use]
    pub fn with_codex_privacy_policy(mut self, policy: super::privacy::CodexPrivacyPolicy) -> Self {
        self.codex_privacy_policy = policy;
        self
    }

    #[must_use]
    pub const fn with_openai_session_affinity_ttl_hours(mut self, hours: u32) -> Self {
        self.openai_session_affinity_ttl_hours = hours;
        self
    }

    #[must_use]
    pub const fn with_openai_account_affinity(
        mut self,
        affinity: crate::account::AccountAffinity,
    ) -> Self {
        self.openai_account_affinity = affinity;
        self
    }

    #[must_use]
    pub const fn with_max_account_rotations(mut self, rotations: u32) -> Self {
        self.max_account_rotations = rotations;
        self
    }

    #[must_use]
    pub const fn with_openai_guardian_reserved_concurrency(mut self, reserved: u32) -> Self {
        self.openai_guardian_reserved_concurrency = reserved;
        self
    }

    #[must_use]
    pub fn request_profiles(&self) -> &BTreeMap<ProviderKind, crate::account::OpaqueProviderData> {
        &self.request_profiles
    }

    #[must_use]
    pub const fn with_smart_scheduling(
        mut self,
        config: crate::account::SmartSchedulingConfig,
    ) -> Self {
        self.smart_scheduling = config;
        self
    }

    #[must_use]
    pub fn with_pricing(
        mut self,
        pricing: impl Into<Arc<crate::metering::PricingOverrides>>,
    ) -> Self {
        self.pricing = pricing.into();
        self
    }

    #[must_use]
    pub fn with_request_profiles(
        mut self,
        profiles: BTreeMap<ProviderKind, crate::account::OpaqueProviderData>,
    ) -> Self {
        self.request_profiles = Arc::new(profiles);
        self
    }

    #[must_use]
    pub const fn with_responses_max_decompressed_body_bytes(mut self, bytes: u64) -> Self {
        self.responses_max_decompressed_body_bytes = bytes;
        self
    }

    #[must_use]
    pub fn with_request_location(
        mut self,
        location: crate::account::RequestLocation,
        enabled: bool,
    ) -> Self {
        self.request_location_enabled = enabled;
        self.request_location = location;
        self
    }

    #[must_use]
    pub const fn with_concurrency_queues(
        mut self,
        max_waiting_per_key: u32,
        max_waiting_per_account: u32,
        timeout_seconds: u32,
    ) -> Self {
        self.max_waiting_per_key = max_waiting_per_key;
        self.max_waiting_per_account = max_waiting_per_account;
        self.concurrency_wait_timeout_seconds = timeout_seconds;
        self
    }

    #[must_use]
    pub fn new(
        max_concurrent_per_account: u32,
        request_interval_ms: u64,
        rotation_strategy: impl Into<String>,
        model_mappings: BTreeMap<String, String>,
        min_codex_desktop_version: Option<String>,
        min_codex_cli_version: Option<String>,
    ) -> Self {
        Self {
            request_profiles: Arc::default(),
            pricing: Arc::default(),
            codex_privacy_policy: Default::default(),
            request_location_enabled: false,
            request_location: crate::account::RequestLocation::default(),
            max_concurrent_per_account,
            max_waiting_per_key: 0,
            max_waiting_per_account: 0,
            concurrency_wait_timeout_seconds: 30,
            openai_guardian_reserved_concurrency: 0,
            openai_account_affinity: crate::account::AccountAffinity::default(),
            max_account_rotations: 3,
            openai_session_affinity_ttl_hours: 24,
            responses_max_decompressed_body_bytes: 64 * 1024 * 1024,
            request_interval_ms,
            smart_scheduling: crate::account::SmartSchedulingConfig::default(),
            rotation_strategy: rotation_strategy.into(),
            model_mappings: Arc::new(model_mappings),
            min_codex_desktop_version,
            min_codex_cli_version,
        }
    }

    #[must_use]
    pub fn with_model_mappings(mut self, mappings: BTreeMap<String, String>) -> Self {
        self.model_mappings = Arc::new(mappings);
        self
    }

    #[must_use]
    pub fn with_min_codex_client_versions(
        mut self,
        versions: crate::policy::CodexClientMinVersions,
    ) -> Self {
        self.min_codex_desktop_version = versions.desktop().map(ToString::to_string);
        self.min_codex_cli_version = versions.cli().map(ToString::to_string);
        self
    }
}

/// 请求排队与账号选择的数值约束；返回不合法的设置字段
pub fn validate_request_limits(
    max_waiting_per_key: u32,
    max_waiting_per_account: u32,
    timeout_seconds: u32,
    max_account_rotations: u32,
    affinity_ttl_hours: u32,
) -> Result<(), &'static str> {
    for (valid, field) in [
        (max_waiting_per_key <= 1_000, "max_waiting_per_key"),
        (max_waiting_per_account <= 1_000, "max_waiting_per_account"),
        (
            (1..=120).contains(&timeout_seconds),
            "concurrency_wait_timeout_seconds",
        ),
        (
            max_account_rotations <= crate::account::MAX_ACCOUNT_ROTATIONS,
            "max_account_rotations",
        ),
        (
            (1..=crate::account::MAX_SESSION_AFFINITY_TTL_HOURS).contains(&affinity_ttl_hours),
            "openai_session_affinity_ttl_hours",
        ),
    ] {
        if !valid {
            return Err(field);
        }
    }
    Ok(())
}

/// 解压上限必须能作为非零缓冲区长度使用
pub fn response_body_limit(bytes: u64) -> Result<std::num::NonZeroUsize, &'static str> {
    isize::try_from(bytes)
        .ok()
        .and_then(|bytes| usize::try_from(bytes).ok())
        .and_then(std::num::NonZeroUsize::new)
        .ok_or("responses_max_decompressed_body_bytes")
}

/// 管理写入与请求覆盖共用客户端最低版本解析
pub fn client_min_versions(
    desktop: Option<&str>,
    cli: Option<&str>,
) -> Result<crate::policy::CodexClientMinVersions, &'static str> {
    use crate::policy::{CodexClientMinVersions, CodexClientVersion};
    Ok(CodexClientMinVersions::new(
        desktop
            .map(CodexClientVersion::parse)
            .transpose()
            .map_err(|_| "min_codex_desktop_version")?,
        cli.map(CodexClientVersion::parse)
            .transpose()
            .map_err(|_| "min_codex_cli_version")?,
    ))
}
