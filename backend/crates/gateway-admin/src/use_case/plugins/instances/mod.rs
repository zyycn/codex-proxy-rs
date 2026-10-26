//! 插件实例用例的职责边界与静态检查结果

mod configuration;
mod migration;
mod mutations;
mod publication;
mod queries;

use super::PluginsService;

#[derive(Clone, Default)]
pub(super) struct PackageStatus {
    pub(super) warning: Option<String>,
    pub(super) load_error: Option<String>,
}
