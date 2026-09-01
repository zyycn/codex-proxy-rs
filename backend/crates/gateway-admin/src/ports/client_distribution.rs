//! 客户端安装包解析的外部能力端口。

use async_trait::async_trait;

use crate::model::client_distribution::CodexDesktopWindowsDownloads;

#[async_trait]
pub trait ClientDistributionResolver: Send + Sync {
    async fn resolve_codex_desktop_windows(&self, refresh: bool) -> CodexDesktopWindowsDownloads;
}
