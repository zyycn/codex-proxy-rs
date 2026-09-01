//! 管理设置页使用的 Codex 客户端下载信息。

use chrono::{DateTime, Utc};

/// Windows 安装包架构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClientArchitecture {
    X64,
    Arm64,
}

impl ClientArchitecture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }
}

/// 下载地址的可信来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientDownloadSource {
    MicrosoftStore,
    OfficialOpenAi,
}

impl ClientDownloadSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MicrosoftStore => "microsoft_store",
            Self::OfficialOpenAi => "official_openai",
        }
    }
}

/// 一个已经过 Host 校验、可以直接交给浏览器下载的安装包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientDownloadPackage {
    pub architecture: ClientArchitecture,
    pub source: ClientDownloadSource,
    pub version: Option<String>,
    pub file_name: String,
    pub size_bytes: Option<u64>,
    pub download_url: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Codex Desktop Windows 下载解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDesktopWindowsDownloads {
    pub resolved_at: DateTime<Utc>,
    pub cached: bool,
    pub warning: Option<String>,
    pub packages: Vec<ClientDownloadPackage>,
}
