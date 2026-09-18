//! 管理端选择的唯一解析入口；持久化配置与官方发布资料分别管理。

use chrono::{DateTime, Utc};
use gateway_core::account::OpaqueProviderData;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CodexWireProfile, CodexWireProfileState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Desktop,
    Cli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientPlatform {
    Macos,
    Linux,
    Windows,
}

impl ClientPlatform {
    pub const fn os_type(self) -> &'static str {
        match self {
            Self::Macos => "Mac OS",
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        }
    }

    fn default_os_version(self) -> &'static str {
        match self {
            Self::Macos => "15.7.1",
            Self::Linux => "6.8.0",
            Self::Windows => "10.0.26100",
        }
    }

    fn default_arch(self) -> &'static str {
        match self {
            Self::Macos => "arm64",
            Self::Linux | Self::Windows => "x86_64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionMode {
    Latest,
    Fixed,
}

/// 空的可选字段表示使用对应预设参数；Key 覆盖始终是一份完整选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientProfileSelection {
    pub client: ClientKind,
    pub platform: ClientPlatform,
    pub version_mode: VersionMode,
    pub originator: Option<String>,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub terminal: Option<String>,
    pub codex_version: Option<String>,
    pub desktop_version: Option<String>,
    pub desktop_build: Option<String>,
}

impl Default for ClientProfileSelection {
    fn default() -> Self {
        Self {
            client: ClientKind::Desktop,
            platform: ClientPlatform::Macos,
            version_mode: VersionMode::Latest,
            originator: None,
            os_version: None,
            arch: None,
            terminal: None,
            codex_version: None,
            desktop_version: None,
            desktop_build: None,
        }
    }
}

impl ClientProfileSelection {
    pub fn parse(document: &OpaqueProviderData) -> Result<Self, ClientProfileError> {
        let selection: Self =
            serde_json::from_value(Value::Object(document.expose_to_provider().clone()))
                .map_err(|_| ClientProfileError::Invalid)?;
        selection.validate()?;
        Ok(selection)
    }

    pub fn document(&self) -> Result<OpaqueProviderData, ClientProfileError> {
        object(self)
    }

    pub fn architecture(&self) -> &str {
        self.arch.as_deref().unwrap_or(self.platform.default_arch())
    }

    pub(super) fn validate(&self) -> Result<(), ClientProfileError> {
        for value in [
            self.originator.as_deref(),
            self.os_version.as_deref(),
            self.arch.as_deref(),
            self.terminal.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.is_empty()
                || value.len() > 128
                || value.trim() != value
                || !value.bytes().all(|byte| (32..=126).contains(&byte))
                || value.contains(['(', ')', ';', '\\'])
            {
                return Err(ClientProfileError::Invalid);
            }
        }
        match self.version_mode {
            VersionMode::Latest
                if self.codex_version.is_some()
                    || self.desktop_version.is_some()
                    || self.desktop_build.is_some() =>
            {
                return Err(ClientProfileError::Invalid);
            }
            VersionMode::Fixed => {
                let version = self
                    .codex_version
                    .as_deref()
                    .ok_or(ClientProfileError::Invalid)?;
                if version.len() > 64 || semver::Version::parse(version).is_err() {
                    return Err(ClientProfileError::Invalid);
                }
                if self.client == ClientKind::Desktop {
                    let version = self
                        .desktop_version
                        .as_deref()
                        .ok_or(ClientProfileError::Invalid)?;
                    if version.len() > 64 || !super::numeric_dotted_version(version) {
                        return Err(ClientProfileError::Invalid);
                    }
                    if self.desktop_build.as_deref().is_none_or(|build| {
                        build.len() > 32
                            || build.is_empty()
                            || !build.bytes().all(|byte| byte.is_ascii_digit())
                    }) {
                        return Err(ClientProfileError::Invalid);
                    }
                }
            }
            VersionMode::Latest => {}
        }
        if self.client == ClientKind::Cli
            && (self.desktop_version.is_some() || self.desktop_build.is_some())
        {
            return Err(ClientProfileError::Invalid);
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<CodexWireProfile, ClientProfileError> {
        self.validate()?;
        let release = match self.version_mode {
            VersionMode::Latest => state
                .client_release(self.client, self.platform, self.architecture())
                .ok_or(ClientProfileError::ReleaseUnavailable)?,
            VersionMode::Fixed => ClientRelease {
                codex_version: self
                    .codex_version
                    .clone()
                    .ok_or(ClientProfileError::Invalid)?,
                desktop_version: self.desktop_version.clone(),
                desktop_build: self.desktop_build.clone(),
                verified_at: None,
            },
        };
        Ok(CodexWireProfile {
            client_kind: self.client,
            originator: self
                .originator
                .clone()
                .unwrap_or_else(|| match self.client {
                    ClientKind::Desktop => "Codex Desktop".to_owned(),
                    ClientKind::Cli => "codex_cli_rs".to_owned(),
                }),
            codex_version: release.codex_version,
            desktop_version: release.desktop_version.unwrap_or_default(),
            desktop_build: release.desktop_build.unwrap_or_default(),
            os_type: self.platform.os_type().to_owned(),
            os_version: self
                .os_version
                .clone()
                .unwrap_or_else(|| self.platform.default_os_version().to_owned()),
            arch: self.architecture().to_owned(),
            terminal: self
                .terminal
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            residency: state.snapshot().residency,
            verified_at: release.verified_at.unwrap_or(DateTime::UNIX_EPOCH),
        })
    }
}

/// 一个具体客户端制品的配套版本；自定义值不携带核验时间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientRelease {
    pub codex_version: String,
    pub desktop_version: Option<String>,
    pub desktop_build: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
}

impl CodexWireProfileState {
    pub fn preview_selection(
        &self,
        configuration: &OpaqueProviderData,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        let selection = ClientProfileSelection::parse(configuration)?;
        let profile = selection.resolve(self)?;
        let status = self.client_release_status(
            selection.client,
            selection.platform,
            selection.architecture(),
        );
        object(&json!({
            "configuration": selection,
            "originator": profile.originator,
            "osType": profile.os_type,
            "osVersion": profile.os_version,
            "arch": profile.arch,
            "terminal": profile.terminal,
            "codexVersion": profile.codex_version,
            "desktopVersion": (profile.client_kind == ClientKind::Desktop).then_some(&profile.desktop_version),
            "desktopBuild": (profile.client_kind == ClientKind::Desktop).then_some(&profile.desktop_build),
            "userAgent": profile.user_agent(),
            "versionSource": if selection.version_mode == VersionMode::Fixed { "custom" } else { "official" },
            "verifiedAt": (profile.verified_at != DateTime::UNIX_EPOCH).then_some(profile.verified_at),
            "checkedAt": status.0,
            "error": status.1,
        }))
    }

    pub fn selection_options(&self) -> Result<OpaqueProviderData, ClientProfileError> {
        let mut presets = Vec::new();
        for platform in [
            ClientPlatform::Macos,
            ClientPlatform::Linux,
            ClientPlatform::Windows,
        ] {
            for client in [ClientKind::Desktop, ClientKind::Cli] {
                let configuration = ClientProfileSelection {
                    client,
                    platform,
                    ..ClientProfileSelection::default()
                };
                let available = configuration.resolve(self).is_ok();
                presets.push(json!({
                    "configuration": configuration,
                    "automaticAvailable": available,
                    "reason": (!available).then_some("暂不支持自动更新"),
                    "defaults": { "originator": if client == ClientKind::Desktop { "Codex Desktop" } else { "codex_cli_rs" }, "osVersion": platform.default_os_version(), "arch": platform.default_arch(), "terminal": "unknown" },
                }));
            }
        }
        object(&json!({ "presets": presets }))
    }
}

pub(crate) fn object(value: &impl Serialize) -> Result<OpaqueProviderData, ClientProfileError> {
    match serde_json::to_value(value).map_err(|_| ClientProfileError::Invalid)? {
        Value::Object(fields) => Ok(OpaqueProviderData::new(fields)),
        _ => Err(ClientProfileError::Invalid),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientProfileError {
    #[error("客户端身份字段或版本组合不合法")]
    Invalid,
    #[error("此客户端、平台与架构尚无已核验发布版本，请选择固定版本或稍后重试")]
    ReleaseUnavailable,
}
