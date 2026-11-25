//! xAI 请求身份快照与运行时更新

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// Grok CLI 运行时身份；默认值作为官方发布检查的内置基线
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct XaiWireProfile {
    pub client_identifier: String,
    pub client_version: String,
    pub client_mode: String,
    pub target_os: String,
    pub target_arch: String,
    pub verified_at: DateTime<Utc>,
}

impl Default for XaiWireProfile {
    fn default() -> Self {
        Self {
            client_identifier: "grok-shell".to_owned(),
            client_version: "1.0.13".to_owned(),
            client_mode: "headless".to_owned(),
            target_os: "linux".to_owned(),
            target_arch: "x86_64".to_owned(),
            verified_at: DateTime::UNIX_EPOCH + chrono::Duration::seconds(1_788_105_600),
        }
    }
}

impl XaiWireProfile {
    #[must_use]
    pub fn user_agent(&self) -> String {
        // Grok CLI 的产品名独立于 x-grok-client-identifier；自定义请求头不替换 UA 产品名
        let arch = match self.target_arch.as_str() {
            "arm64" => "aarch64",
            arch => arch,
        };
        format!(
            "grok-shell/{} ({}; {arch})",
            self.client_version, self.target_os
        )
    }
}

#[derive(Debug, Clone)]
pub struct XaiWireProfileState(Arc<RwLock<XaiWireProfile>>);

impl XaiWireProfileState {
    /// 账号后台操作按当前全局配置冻结身份，不读取 Client Key 覆盖
    pub(crate) async fn resolve_global_request_profile(
        &self,
        policy: &dyn gateway_core::provider_ports::ProviderRuntimePolicyPort,
    ) -> Result<XaiWireProfile, gateway_core::error::ErrorSource> {
        let kind = gateway_core::routing::ProviderKind::new("xai")?;
        let Some(configuration) = policy.load_request_profile(&kind).await? else {
            return Ok(self.snapshot());
        };
        super::selection::GrokClientProfileSelection::parse(&configuration)
            .and_then(|selection| selection.resolve(self))
            .map_err(Into::into)
    }

    #[must_use]
    pub fn new(profile: XaiWireProfile) -> Self {
        Self(Arc::new(RwLock::new(profile)))
    }

    #[must_use]
    pub fn client_identifier(&self) -> String {
        self.snapshot().client_identifier
    }
    #[must_use]
    pub fn client_version(&self) -> String {
        self.snapshot().client_version
    }
    #[must_use]
    pub fn client_mode(&self) -> String {
        self.snapshot().client_mode
    }
    #[must_use]
    pub fn target_os(&self) -> String {
        self.snapshot().target_os
    }
    #[must_use]
    pub fn target_arch(&self) -> String {
        self.snapshot().target_arch
    }
    #[must_use]
    pub fn verified_at(&self) -> DateTime<Utc> {
        self.snapshot().verified_at
    }

    #[must_use]
    pub fn user_agent(&self) -> String {
        self.snapshot().user_agent()
    }

    #[must_use]
    pub fn snapshot(&self) -> XaiWireProfile {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn update_client_version(&self, version: &str) {
        let mut profile = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if profile.client_version != version {
            profile.client_version = version.to_owned();
            profile.verified_at = Utc::now();
        }
    }
}
