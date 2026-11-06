//! 预设与完整自定义 UA 的统一解析边界，身份配套头由 Provider 所有

use chrono::DateTime;
use gateway_core::account::OpaqueProviderData;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::selection::{
    ClientKind, ClientProfileError, ClientProfileSelection, VersionMode, object,
};
use super::{CodexWireProfile, CodexWireProfileState};

/// 已解析的配置；没有 mode 的预设配置复用官方版本与平台合同
#[derive(Debug, Clone)]
pub struct RequestProfileSelection {
    selection: ParsedSelection,
    installation_seed: Option<String>,
}

#[derive(Debug, Clone)]
enum ParsedSelection {
    Preset(ClientProfileSelection),
    Custom(ExactIdentity),
}

impl CodexWireProfile {
    pub(crate) fn effective_installation_id(
        &self,
        client_key_id: &gateway_core::policy::ClientApiKeyId,
        account_id: &gateway_core::account::ProviderAccountId,
        account_installation_id: &str,
    ) -> String {
        let Some(seed) = &self.installation_seed else {
            return account_installation_id.to_owned();
        };
        // 稳定记录 ID 不受 UA、令牌刷新或账号凭据中的安装身份变化影响
        // 长度前缀避免字段边界歧义，独立用途域不与会话关联标识共享派生空间
        let mut digest = Sha256::new();
        digest.update(b"codex-key-account-installation-v1\0");
        for part in [seed.as_str(), client_key_id.as_str(), account_id.as_str()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
        let hash = digest.finalize();
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&hash[..16]);
        // 沿用出站关联标识的 UUID 形状，值由种子确定，不按请求重新随机生成
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum SelectionDocument {
    Custom {
        #[serde(rename = "userAgent")]
        user_agent: String,
        originator: Option<String>,
        #[serde(rename = "codexVersion")]
        codex_version: Option<String>,
    },
}

pub(crate) fn inherited_identity_fields(
    document: &OpaqueProviderData,
) -> Result<Option<&serde_json::Map<String, Value>>, ClientProfileError> {
    let inherited = gateway_core::settings::request_profiles::inherited_fields(document);
    if inherited.is_some_and(|fields| fields.len() != 1 || !fields.contains_key("installationSeed"))
    {
        return Err(ClientProfileError::Invalid);
    }
    Ok(inherited)
}

impl RequestProfileSelection {
    pub fn preset(&self) -> Option<&ClientProfileSelection> {
        match &self.selection {
            ParsedSelection::Preset(selection) => Some(selection),
            _ => None,
        }
    }

    pub fn version_mode(&self) -> VersionMode {
        match &self.selection {
            ParsedSelection::Preset(selection) => selection.version_mode,
            ParsedSelection::Custom(_) => VersionMode::Fixed,
        }
    }

    pub fn parse(document: &OpaqueProviderData) -> Result<Self, ClientProfileError> {
        let inherited = inherited_identity_fields(document)?;
        let mut fields = inherited
            .unwrap_or_else(|| document.expose_to_provider())
            .clone();
        let installation_seed = match fields.remove("installationSeed") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value.is_empty() => None,
            Some(Value::String(value)) => {
                let parsed = uuid::Uuid::try_parse(&value)
                    .map_err(|_| ClientProfileError::InvalidInstallationSeed)?;
                if value.len() != uuid::fmt::Hyphenated::LENGTH {
                    return Err(ClientProfileError::InvalidInstallationSeed);
                }
                Some(parsed.to_string())
            }
            Some(_) => return Err(ClientProfileError::InvalidInstallationSeed),
        };
        let selection = if inherited.is_some() {
            ParsedSelection::Preset(ClientProfileSelection::default())
        } else if fields.contains_key("mode") {
            match serde_json::from_value(Value::Object(fields))
                .map_err(|_| ClientProfileError::Invalid)?
            {
                SelectionDocument::Custom {
                    user_agent,
                    originator,
                    codex_version,
                } => ParsedSelection::Custom(ExactIdentity::parse(
                    &user_agent,
                    originator.as_deref(),
                    codex_version.as_deref(),
                )?),
            }
        } else {
            ParsedSelection::Preset(ClientProfileSelection::from_fields(fields)?)
        };
        Ok(Self {
            selection,
            installation_seed,
        })
    }

    pub fn resolve(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<CodexWireProfile, ClientProfileError> {
        let mut profile = match &self.selection {
            ParsedSelection::Preset(selection) => selection.resolve(state)?,
            ParsedSelection::Custom(identity) => identity.profile(state),
        };
        profile
            .installation_seed
            .clone_from(&self.installation_seed);
        Ok(profile)
    }

    pub(super) fn preview(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        let preview = match &self.selection {
            ParsedSelection::Preset(selection) => state.preview_preset_selection(selection)?,
            ParsedSelection::Custom(identity) => identity.preview(state)?,
        };
        let Some(seed) = &self.installation_seed else {
            return Ok(preview);
        };
        let mut fields = preview.into_inner();
        fields["configuration"]["installationSeed"] = Value::String(seed.clone());
        Ok(OpaqueProviderData::new(fields))
    }
}

#[derive(Debug, Clone)]
struct ExactIdentity {
    user_agent: String,
    originator: String,
    codex_version: String,
    recognized: bool,
    environment: Option<UaEnvironment>,
    desktop_version: Option<String>,
}

impl ExactIdentity {
    fn parse(
        user_agent: &str,
        originator: Option<&str>,
        codex_version: Option<&str>,
    ) -> Result<Self, ClientProfileError> {
        if !safe_header(user_agent, 4096) {
            return Err(ClientProfileError::InvalidUserAgent);
        }
        let known = ["Codex Desktop", "codex-tui", "codex_exec", "codex_cli_rs"]
            .into_iter()
            .find_map(|name| {
                user_agent
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|rest| (name, rest))
            });
        let (originator, codex_version, recognized) = if let Some((name, rest)) = known {
            let version = rest.split_once(' ').map_or(rest, |(version, _)| version);
            parse_core_version(version)?;
            if originator.is_some_and(|value| value != name)
                || codex_version.is_some_and(|value| value != version)
            {
                return Err(ClientProfileError::CompanionHeadersConflict);
            }
            (name.to_owned(), version.to_owned(), true)
        } else {
            let originator = originator
                .filter(|value| safe_header(value, 128))
                .ok_or(ClientProfileError::CompanionHeadersRequired)?;
            let version = codex_version.ok_or(ClientProfileError::CompanionHeadersRequired)?;
            parse_core_version(version)?;
            (originator.to_owned(), version.to_owned(), false)
        };
        let environment = known.and_then(|(_, rest)| UaEnvironment::parse(rest));
        let desktop_version = (originator == "Codex Desktop")
            .then(|| {
                user_agent
                    .rsplit_once(" (Codex Desktop; ")
                    .and_then(|(_, version)| version.strip_suffix(')'))
                    .filter(|version| super::numeric_dotted_version(version))
                    .map(ToOwned::to_owned)
            })
            .flatten();
        Ok(Self {
            user_agent: user_agent.to_owned(),
            originator,
            codex_version,
            recognized,
            environment,
            desktop_version,
        })
    }

    fn profile(&self, state: &CodexWireProfileState) -> CodexWireProfile {
        let environment = self.environment.as_ref();
        let deployment = state.snapshot();
        CodexWireProfile {
            client_kind: if self.originator == "Codex Desktop" {
                ClientKind::Desktop
            } else {
                ClientKind::Cli
            },
            originator: self.originator.clone(),
            codex_version: self.codex_version.clone(),
            desktop_version: self.desktop_version.clone().unwrap_or_default(),
            desktop_build: String::new(),
            os_type: environment
                .map(|value| value.os_type.clone())
                .unwrap_or_default(),
            os_version: environment
                .map(|value| value.os_version.clone())
                .unwrap_or_default(),
            arch: environment
                .map(|value| value.arch.clone())
                .unwrap_or_default(),
            terminal: environment
                .map(|value| value.terminal.clone())
                .unwrap_or_default(),
            exact_user_agent: Some(self.user_agent.clone()),
            installation_seed: None,
            residency: deployment.residency,
            account_routing_override: deployment.account_routing_override,
            verified_at: DateTime::UNIX_EPOCH,
        }
    }

    fn preview(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        let profile = self.profile(state);
        object(&json!({
            "configuration": {
                "mode": "custom", "userAgent": self.user_agent,
                "originator": self.originator, "codexVersion": self.codex_version,
            },
            "originator": profile.originator,
            "osType": profile.os_type,
            "osVersion": profile.os_version,
            "arch": profile.arch,
            "terminal": profile.terminal,
            "codexVersion": profile.codex_version,
            "desktopVersion": self.desktop_version,
            "desktopBuild": null,
            "userAgent": profile.user_agent(),
            "recognized": self.recognized,
            "versionSource": "custom",
            "verifiedAt": null,
            "checkedAt": null,
            "error": null,
        }))
    }
}

#[derive(Debug, Clone)]
struct UaEnvironment {
    os_type: String,
    os_version: String,
    arch: String,
    terminal: String,
}

impl UaEnvironment {
    fn parse(rest: &str) -> Option<Self> {
        let (_, rest) = rest.split_once(" (")?;
        let (environment, tail) = rest.split_once(") ")?;
        let (os, arch) = environment.split_once("; ")?;
        let (os_type, os_version) = os.rsplit_once(' ')?;
        let terminal = tail.split_once(" (").map_or(tail, |(terminal, _)| terminal);
        if os_type.is_empty() || os_version.is_empty() || arch.is_empty() || terminal.is_empty() {
            return None;
        }
        Some(Self {
            os_type: os_type.to_owned(),
            os_version: os_version.to_owned(),
            arch: arch.to_owned(),
            terminal: terminal.to_owned(),
        })
    }
}

fn safe_header(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && value.bytes().all(|byte| (32..=126).contains(&byte))
}

fn parse_core_version(value: &str) -> Result<semver::Version, ClientProfileError> {
    if value.len() > 64 {
        return Err(ClientProfileError::Invalid);
    }
    semver::Version::parse(value).map_err(|_| ClientProfileError::Invalid)
}
