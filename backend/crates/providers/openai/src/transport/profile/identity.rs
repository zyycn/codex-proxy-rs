//! 预设与完整自定义 UA 的统一解析边界，身份配套头由 Provider 所有

use chrono::DateTime;
use gateway_core::account::OpaqueProviderData;
use serde::Deserialize;
use serde_json::{Value, json};

use super::selection::{
    ClientKind, ClientProfileError, ClientProfileSelection, VersionMode, object,
};
use super::{CodexWireProfile, CodexWireProfileState};

/// 已解析的配置；没有 mode 的预设配置复用官方版本与平台合同
#[derive(Debug, Clone)]
pub struct RequestProfileSelection(ParsedSelection);

#[derive(Debug, Clone)]
enum ParsedSelection {
    Preset(ClientProfileSelection),
    Custom(ExactIdentity),
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

impl RequestProfileSelection {
    pub fn preset(&self) -> Option<&ClientProfileSelection> {
        match &self.0 {
            ParsedSelection::Preset(selection) => Some(selection),
            _ => None,
        }
    }

    pub fn version_mode(&self) -> VersionMode {
        match &self.0 {
            ParsedSelection::Preset(selection) => selection.version_mode,
            ParsedSelection::Custom(_) => VersionMode::Fixed,
        }
    }

    pub fn parse(document: &OpaqueProviderData) -> Result<Self, ClientProfileError> {
        let fields = document.expose_to_provider();
        if !fields.contains_key("mode") {
            return ClientProfileSelection::parse(document)
                .map(ParsedSelection::Preset)
                .map(Self);
        }
        let document = serde_json::from_value(Value::Object(fields.clone()))
            .map_err(|_| ClientProfileError::Invalid)?;
        let selection = match document {
            SelectionDocument::Custom {
                user_agent,
                originator,
                codex_version,
            } => ParsedSelection::Custom(ExactIdentity::parse(
                &user_agent,
                originator.as_deref(),
                codex_version.as_deref(),
            )?),
        };
        Ok(Self(selection))
    }

    pub fn resolve(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<CodexWireProfile, ClientProfileError> {
        match &self.0 {
            ParsedSelection::Preset(selection) => selection.resolve(state),
            ParsedSelection::Custom(identity) => Ok(identity.profile(state)),
        }
    }

    pub(super) fn preview(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        match &self.0 {
            ParsedSelection::Preset(selection) => state.preview_preset_selection(selection),
            ParsedSelection::Custom(identity) => identity.preview(state),
        }
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
            residency: state.snapshot().residency,
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
