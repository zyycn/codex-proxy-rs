//! Codex 客户端下载信息用例。

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    model::client_distribution::CodexDesktopWindowsDownloads,
    ports::client_distribution::ClientDistributionResolver,
};

#[async_trait]
pub trait ClientDistributionService: Send + Sync {
    async fn codex_desktop_windows(&self, refresh: bool) -> CodexDesktopWindowsDownloads;
}

pub(crate) struct DefaultClientDistributionService {
    resolver: Arc<dyn ClientDistributionResolver>,
}

impl DefaultClientDistributionService {
    #[must_use]
    pub(crate) const fn new(resolver: Arc<dyn ClientDistributionResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl ClientDistributionService for DefaultClientDistributionService {
    async fn codex_desktop_windows(&self, refresh: bool) -> CodexDesktopWindowsDownloads {
        self.resolver.resolve_codex_desktop_windows(refresh).await
    }
}
