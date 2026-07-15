//! Codex Desktop 上游请求画像。

use chrono::{DateTime, Utc};

/// 经制品与 Codex 源码核验后固定的上游请求身份。
///
/// 该画像是启动配置，不会被在线发布检查自动改写。Desktop 发布变化后应先重新审计
/// bundled Codex Core，再整体更新此画像。
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
