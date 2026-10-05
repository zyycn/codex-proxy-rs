//! 有期限的插件合同适配；旧字段只在此边界出现，不进入 Core 或持久化模型

mod fast;

pub(crate) use fast::FastSettings;

use std::sync::OnceLock;

use gateway_admin::model::{AdminError, plugins::instances::PluginApiDeprecation};
use gateway_plugin_sdk::{Capability, Manifest};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Deprecation {
    capability: Capability,
    version: u32,
    replacement_version: u32,
    introduced_in: Option<String>,
    #[serde(rename = "last_counted_in")]
    _last_counted_in: Option<String>,
    remaining_releases: u32,
    migration: String,
}

fn declarations() -> Result<&'static [Deprecation], AdminError> {
    static DECLARATIONS: OnceLock<Result<Vec<Deprecation>, serde_json::Error>> = OnceLock::new();
    DECLARATIONS
        .get_or_init(|| serde_json::from_str(include_str!("../../plugin-api-deprecations.json")))
        .as_deref()
        .map_err(|_| AdminError::internal("插件接口弃用声明不合法"))
}

pub(crate) fn warnings<'a>(
    versions: impl IntoIterator<Item = (&'a str, u32)>,
) -> Result<Vec<PluginApiDeprecation>, AdminError> {
    let declarations = declarations()?;
    Ok(versions
        .into_iter()
        .filter_map(|(capability, version)| {
            declarations.iter().find(|entry| {
                entry.capability.identifier() == capability && entry.version == version
            })
        })
        .map(|entry| PluginApiDeprecation {
            capability: entry.capability.identifier().to_owned(),
            version: entry.version,
            replacement_version: entry.replacement_version,
            introduced_in: entry.introduced_in.clone(),
            remaining_releases: entry.remaining_releases,
            migration: entry.migration.clone(),
        })
        .collect())
}

pub(crate) fn report(manifest: &Manifest, instance_id: &str) -> Result<(), AdminError> {
    for warning in warnings(
        manifest
            .contributes
            .iter()
            .map(|(capability, declaration)| (capability.identifier(), declaration.version)),
    )? {
        // 只在实例加载时报告，不在每次请求或管理页轮询时重复记录
        tracing::warn!(
            instance_id,
            capability = warning.capability,
            version = warning.version,
            replacement_version = warning.replacement_version,
            remaining_releases = warning.remaining_releases,
            migration = warning.migration,
            "插件接口已弃用，当前仍兼容，请升级插件"
        );
    }
    Ok(())
}
