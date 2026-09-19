//! Grok CLI 管理配置与请求级身份解析；发布版本与用户选择分别维护。

use gateway_core::account::OpaqueProviderData;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{XaiWireProfile, XaiWireProfileState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionMode {
    Latest,
    Fixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrokClientProfileSelection {
    pub version_mode: VersionMode,
    pub client_version: Option<String>,
    pub client_identifier: String,
    pub client_mode: String,
    pub target_os: String,
    pub target_arch: String,
}

impl Default for GrokClientProfileSelection {
    fn default() -> Self {
        let profile = XaiWireProfile::default();
        Self {
            version_mode: VersionMode::Latest,
            client_version: None,
            client_identifier: profile.client_identifier,
            client_mode: profile.client_mode,
            target_os: profile.target_os,
            target_arch: profile.target_arch,
        }
    }
}

impl GrokClientProfileSelection {
    pub fn parse(document: &OpaqueProviderData) -> Result<Self, ClientProfileError> {
        let selection: Self =
            serde_json::from_value(Value::Object(document.expose_to_provider().clone()))
                .map_err(|_| ClientProfileError)?;
        selection.validate()?;
        Ok(selection)
    }

    pub fn document(&self) -> Result<OpaqueProviderData, ClientProfileError> {
        self.validate()?;
        object(self)
    }

    fn validate(&self) -> Result<(), ClientProfileError> {
        for value in [
            &self.client_identifier,
            &self.client_mode,
            &self.target_os,
            &self.target_arch,
        ] {
            if value.is_empty()
                || value.len() > 64
                || !value.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(ClientProfileError);
            }
        }
        match (self.version_mode, self.client_version.as_deref()) {
            (VersionMode::Latest, None) => Ok(()),
            (VersionMode::Fixed, Some(version))
                if version.len() <= 64 && semver::Version::parse(version).is_ok() =>
            {
                Ok(())
            }
            _ => Err(ClientProfileError),
        }
    }

    pub fn resolve(
        &self,
        state: &XaiWireProfileState,
    ) -> Result<XaiWireProfile, ClientProfileError> {
        self.validate()?;
        let mut profile = state.snapshot();
        if let Some(version) = &self.client_version {
            profile.client_version.clone_from(version);
            profile.verified_at = chrono::DateTime::UNIX_EPOCH;
        }
        profile
            .client_identifier
            .clone_from(&self.client_identifier);
        profile.client_mode.clone_from(&self.client_mode);
        profile.target_os.clone_from(&self.target_os);
        profile.target_arch.clone_from(&self.target_arch);
        Ok(profile)
    }

    pub fn preview(
        &self,
        state: &XaiWireProfileState,
        status: &super::profile::GrokCliReleaseSnapshot,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        let profile = self.resolve(state)?;
        let latest = self.version_mode == VersionMode::Latest;
        object(&json!({
            "configuration": self,
            "clientVersion": profile.client_version,
            "clientIdentifier": profile.client_identifier,
            "clientMode": profile.client_mode,
            "targetOs": profile.target_os,
            "targetArch": profile.target_arch,
            "userAgent": profile.user_agent(),
            "versionSource": if latest { "official" } else { "custom" },
            "verifiedAt": latest.then_some(profile.verified_at),
            "checkedAt": latest.then_some(status.checked_at).flatten(),
            "error": if latest { status.last_error.as_deref() } else { None },
        }))
    }
}

pub fn object(value: &impl Serialize) -> Result<OpaqueProviderData, ClientProfileError> {
    match serde_json::to_value(value).map_err(|_| ClientProfileError)? {
        Value::Object(fields) => Ok(OpaqueProviderData::new(fields)),
        _ => Err(ClientProfileError),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Grok CLI 客户端身份字段或版本组合不合法")]
pub struct ClientProfileError;
