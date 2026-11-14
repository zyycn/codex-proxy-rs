//! OpenAI 客户端的离线版本、制品身份与预设环境默认值
//!
//! 更新时一并核验配套版本、构建号、时间及各架构制品身份
//! 各平台分别核验，版本相同不表示可以借用另一平台的资料

use chrono::{DateTime, Utc};

pub(super) const DESKTOP_ORIGINATOR: &str = "Codex Desktop";
pub(super) const CLI_ORIGINATOR: &str = "codex_cli_rs";
pub(super) const TUI_ORIGINATOR: &str = "codex-tui";
pub(super) const EXEC_ORIGINATOR: &str = "codex_exec";
pub(super) const TERMINAL: &str = "unknown";

pub(super) struct Environment {
    pub os_type: &'static str,
    pub os_version: &'static str,
    pub arch: &'static str,
}

pub(super) const MACOS_ENVIRONMENT: Environment = Environment {
    os_type: "Mac OS",
    os_version: "15.7.1",
    arch: "arm64",
};
pub(super) const LINUX_ENVIRONMENT: Environment = Environment {
    os_type: "Linux",
    os_version: "6.8.0",
    arch: "x86_64",
};
pub(super) const WINDOWS_ENVIRONMENT: Environment = Environment {
    os_type: "Windows",
    os_version: "10.0.26100",
    arch: "x86_64",
};

pub(super) struct CoreRelease {
    pub version: &'static str,
    pub verified_at_millis: i64,
}

impl CoreRelease {
    pub fn verified_at(&self) -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + chrono::Duration::milliseconds(self.verified_at_millis)
    }
}

pub(super) struct DesktopRelease {
    pub core: CoreRelease,
    pub version: &'static str,
    pub build: &'static str,
}

pub(super) struct DesktopArtifact {
    pub arch: &'static str,
    pub size: u64,
    pub etag: &'static str,
}

// 官方 npm 稳定标签及六个平台依赖；核验于 2026-11-14T02:09:52Z
pub(super) const CLI_RELEASE: CoreRelease = CoreRelease {
    version: "0.169.0",
    verified_at_millis: 1_794_622_192_000,
};

// macOS arm64 ZIP；核验于 2026-11-14T02:09:52Z
pub(super) const MACOS_RELEASE: DesktopRelease = DesktopRelease {
    core: CoreRelease {
        version: "0.169.0-alpha.11.1",
        verified_at_millis: 1_794_622_192_000,
    },
    version: "26.1111.21308",
    build: "27194",
};

// Windows 两种架构的 MSIX；核验于 2026-11-14T02:09:52Z
pub(super) const WINDOWS_RELEASE: DesktopRelease = DesktopRelease {
    core: CoreRelease {
        version: "0.167.0-alpha.4.1",
        verified_at_millis: 1_794_622_192_000,
    },
    version: "26.1106.51428",
    build: "26605",
};
pub(super) const WINDOWS_ARTIFACTS: [DesktopArtifact; 2] = [
    DesktopArtifact {
        arch: "x86_64",
        size: 1_057_680_859,
        etag: "\"0x8DF3FEB5404E49A\"",
    },
    DesktopArtifact {
        arch: "arm64",
        size: 1_158_775_140,
        etag: "\"0x8DF3FEB5ABF50ED\"",
    },
];

// Linux 两种架构的 DEB；核验于 2026-11-14T02:09:52Z
pub(super) const LINUX_RELEASE: DesktopRelease = DesktopRelease {
    core: CoreRelease {
        version: "0.169.0-alpha.11.1",
        verified_at_millis: 1_794_622_192_000,
    },
    version: "26.1111.21308",
    build: "27194",
};
pub(super) const LINUX_ARTIFACTS: [DesktopArtifact; 2] = [
    DesktopArtifact {
        arch: "x86_64",
        size: 567_877_422,
        etag: "\"0x8DF4126A0871331\"",
    },
    DesktopArtifact {
        arch: "arm64",
        size: 553_033_238,
        etag: "\"0x8DF41269D464E6A\"",
    },
];
