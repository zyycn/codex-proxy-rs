//! Codex Desktop 上游请求画像。

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};

/// Codex Desktop 上游请求身份。
///
/// 启动配置提供经源码审计的 Core、运行环境和初始 Desktop 版本。官方 appcast
/// 检查成功后，会仅同步 Desktop 版本和构建号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWireProfile {
    /// `originator` 请求头及 User-Agent 产品名。
    pub originator: String,
    /// bundled Codex Core 版本；同时用于 `/codex/models?client_version=`。
    pub codex_version: String,
    /// Desktop 应用版本，用于 app-server `clientInfo.version` 对应的 UA 后缀。
    pub desktop_version: String,
    /// Desktop 制品构建号，仅用于发布对齐诊断。
    pub desktop_build: String,
    /// Codex Core UA 中的目标操作系统类型。
    pub os_type: String,
    /// Codex Core UA 中的目标操作系统版本。
    pub os_version: String,
    /// Codex Core UA 中的目标架构。
    pub arch: String,
    /// Codex Core UA 中的终端标记。
    pub terminal: String,
    /// 此画像最后一次经制品与源码核验的时间。
    pub verified_at: DateTime<Utc>,
}

impl CodexWireProfile {
    /// 按 Codex Core 的官方格式生成最终 User-Agent。
    pub fn user_agent(&self) -> String {
        format!(
            "{}/{} ({} {}; {}) {} ({}; {})",
            self.originator,
            self.codex_version,
            self.os_type,
            self.os_version,
            self.arch,
            self.terminal,
            self.originator,
            self.desktop_version,
        )
    }
}

/// 跨上游请求与发布检查共享的运行时请求画像。
#[derive(Debug, Clone)]
pub struct CodexWireProfileState {
    profile: Arc<RwLock<CodexWireProfile>>,
}

impl CodexWireProfileState {
    /// 从启动画像创建运行时状态。
    pub fn new(profile: CodexWireProfile) -> Self {
        Self {
            profile: Arc::new(RwLock::new(profile)),
        }
    }

    /// 返回当前画像的独立快照，避免持锁执行网络请求。
    pub fn snapshot(&self) -> CodexWireProfile {
        self.profile
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 使用官方 appcast 的 Desktop 版本和构建号更新当前画像。
    pub fn update_desktop_release(&self, desktop_version: &str, desktop_build: &str) {
        let mut profile = self
            .profile
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if profile.desktop_version == desktop_version && profile.desktop_build == desktop_build {
            return;
        }
        profile.desktop_version = desktop_version.to_string();
        profile.desktop_build = desktop_build.to_string();
    }
}
