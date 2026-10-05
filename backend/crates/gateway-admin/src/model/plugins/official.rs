//! 官方插件发行身份与导入命令

use super::super::Revision;

/// 编译进宿主二进制的发行身份；不能由运行时配置或插件清单覆盖
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficialPluginReleaseIdentity {
    pub gateway_version: String,
    pub gateway_git_sha: String,
}

/// 启动时导入官方制品的结果；导入只持久化制品，不创建或启用实例
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfficialPluginImport {
    pub artifacts: usize,
    pub config_revision: Option<Revision>,
}
