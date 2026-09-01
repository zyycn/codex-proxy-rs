//! Codex 客户端最低版本策略。

use std::fmt;

use semver::Version;

const MAXIMUM_VERSION_LENGTH: usize = 64;

/// 可被网关识别并限制最低版本的 Codex 客户端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexClientKind {
    Desktop,
    Cli,
}

impl CodexClientKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "codex_desktop",
            Self::Cli => "codex_cli",
        }
    }
}

/// 经严格 SemVer 校验的 Codex 客户端版本。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CodexClientVersion(Version);

impl CodexClientVersion {
    /// 仅接受没有前后空白、长度受限的标准 SemVer。
    pub fn parse(value: &str) -> Result<Self, CodexClientVersionError> {
        if value.is_empty()
            || value.len() > MAXIMUM_VERSION_LENGTH
            || value.trim() != value
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(CodexClientVersionError);
        }
        Version::parse(value)
            .map(Self)
            .map_err(|_| CodexClientVersionError)
    }
}

impl fmt::Display for CodexClientVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// 客户端版本 wire 值不合法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Codex client version must be a valid semantic version")]
pub struct CodexClientVersionError;

/// 运行快照中冻结的最低版本要求；`None` 表示该客户端不限制。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexClientMinVersions {
    desktop: Option<CodexClientVersion>,
    cli: Option<CodexClientVersion>,
}

impl CodexClientMinVersions {
    #[must_use]
    pub const fn new(desktop: Option<CodexClientVersion>, cli: Option<CodexClientVersion>) -> Self {
        Self { desktop, cli }
    }

    #[must_use]
    pub const fn desktop(&self) -> Option<&CodexClientVersion> {
        self.desktop.as_ref()
    }

    #[must_use]
    pub const fn cli(&self) -> Option<&CodexClientVersion> {
        self.cli.as_ref()
    }

    #[must_use]
    pub const fn min_for(&self, kind: CodexClientKind) -> Option<&CodexClientVersion> {
        match kind {
            CodexClientKind::Desktop => self.desktop(),
            CodexClientKind::Cli => self.cli(),
        }
    }

    /// 校验一个已识别客户端；配置未启用时直接放行。
    pub fn enforce(
        &self,
        kind: CodexClientKind,
        current: Option<&CodexClientVersion>,
    ) -> Result<(), ClientVersionRejection> {
        let Some(min) = self.min_for(kind) else {
            return Ok(());
        };
        let Some(current) = current else {
            return Err(ClientVersionRejection::Unavailable {
                kind,
                min: min.clone(),
            });
        };
        if current < min {
            return Err(ClientVersionRejection::TooOld {
                kind,
                current: current.clone(),
                min: min.clone(),
            });
        }
        Ok(())
    }
}

/// 已识别客户端未满足冻结的最低版本要求。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientVersionRejection {
    #[error("recognized Codex client did not provide a valid version")]
    Unavailable {
        kind: CodexClientKind,
        min: CodexClientVersion,
    },
    #[error("recognized Codex client is below the required minimum version")]
    TooOld {
        kind: CodexClientKind,
        current: CodexClientVersion,
        min: CodexClientVersion,
    },
}

impl ClientVersionRejection {
    #[must_use]
    pub const fn kind(&self) -> CodexClientKind {
        match self {
            Self::Unavailable { kind, .. } | Self::TooOld { kind, .. } => *kind,
        }
    }

    #[must_use]
    pub const fn current(&self) -> Option<&CodexClientVersion> {
        match self {
            Self::Unavailable { .. } => None,
            Self::TooOld { current, .. } => Some(current),
        }
    }

    #[must_use]
    pub const fn min(&self) -> &CodexClientVersion {
        match self {
            Self::Unavailable { min, .. } | Self::TooOld { min, .. } => min,
        }
    }
}
