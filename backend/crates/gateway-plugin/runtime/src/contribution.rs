//! 将实例能力绑定解析为清单中的唯一贡献声明

use gateway_admin::model::{AdminError, plugins::instances::PluginCapabilityBinding};
use gateway_plugin_sdk::{Capability, ContributionDeclaration, Manifest};

pub(super) struct ResolvedContribution<'a> {
    pub(super) capability: Capability,
    pub(super) declaration: &'a ContributionDeclaration,
}

/// 绑定只保存贡献项 ID；实际能力始终来自已检查清单的 map key
pub(super) fn resolve<'a>(
    manifest: &'a Manifest,
    binding: &PluginCapabilityBinding,
) -> Result<ResolvedContribution<'a>, AdminError> {
    manifest
        .contributes
        .iter()
        .find(|(_, declaration)| declaration.id == binding.contribution)
        .map(|(capability, declaration)| ResolvedContribution {
            capability: *capability,
            declaration,
        })
        .ok_or_else(|| AdminError::invalid("插件绑定引用了未声明的贡献项"))
}
