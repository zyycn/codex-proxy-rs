//! OpenAI Provider 启动配置与 Codex Desktop 请求画像校验。

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::transport::profile::{CodexWireProfile, CodexWireProfileState};

/// OpenAI Provider 唯一启动配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenAiConfig {
    pub wire_profile: CodexWireProfileConfig,
}

impl OpenAiConfig {
    /// 校验 Provider-owned 字段；当前配置不含文件系统路径。
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), OpenAiConfigError> {
        self.wire_profile.validate()
    }

    #[must_use]
    pub fn wire_profile_state(&self) -> CodexWireProfileState {
        CodexWireProfileState::new(self.wire_profile.clone().into())
    }
}

/// 经审计固定的 Codex Desktop 上游请求画像。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexWireProfileConfig {
    pub originator: String,
    pub codex_version: String,
    pub desktop_version: String,
    pub desktop_build: String,
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
    pub verified_at: DateTime<Utc>,
}

impl CodexWireProfileConfig {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        for (field, value) in [
            ("openai.wire_profile.originator", self.originator.as_str()),
            (
                "openai.wire_profile.codex_version",
                self.codex_version.as_str(),
            ),
            (
                "openai.wire_profile.desktop_version",
                self.desktop_version.as_str(),
            ),
            (
                "openai.wire_profile.desktop_build",
                self.desktop_build.as_str(),
            ),
            ("openai.wire_profile.os_type", self.os_type.as_str()),
            ("openai.wire_profile.os_version", self.os_version.as_str()),
            ("openai.wire_profile.arch", self.arch.as_str()),
            ("openai.wire_profile.terminal", self.terminal.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(OpenAiConfigError::InvalidField(field));
            }
        }
        if semver::Version::parse(&self.codex_version).is_err() {
            return Err(OpenAiConfigError::InvalidField(
                "openai.wire_profile.codex_version",
            ));
        }
        if !numeric_dotted_version(&self.desktop_version) {
            return Err(OpenAiConfigError::InvalidField(
                "openai.wire_profile.desktop_version",
            ));
        }
        if !self.desktop_build.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(OpenAiConfigError::InvalidField(
                "openai.wire_profile.desktop_build",
            ));
        }
        Ok(())
    }
}

impl From<CodexWireProfileConfig> for CodexWireProfile {
    fn from(value: CodexWireProfileConfig) -> Self {
        Self {
            originator: value.originator,
            codex_version: value.codex_version,
            desktop_version: value.desktop_version,
            desktop_build: value.desktop_build,
            os_type: value.os_type,
            os_version: value.os_version,
            arch: value.arch,
            terminal: value.terminal,
            verified_at: value.verified_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenAiConfigError {
    #[error("OpenAI configuration field is invalid: {0}")]
    InvalidField(&'static str),
}

fn numeric_dotted_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid_parts = parts
        .by_ref()
        .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .count();
    valid_parts >= 2 && valid_parts == value.split('.').count()
}
