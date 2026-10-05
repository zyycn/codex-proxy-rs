//! 插件制品、实例、来源、自有资源与私有状态的 PostgreSQL 适配入口

mod artifacts;
mod credentials;
mod instances;
mod mutation;
mod resources;
mod sources;
mod state;

pub use artifacts::PgPluginStore;

pub(super) use mutation::begin_plugin_mutation;
