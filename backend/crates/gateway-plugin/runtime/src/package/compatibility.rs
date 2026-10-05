//! 读取宿主插件兼容性声明，并校验清单所需协议与能力版本

use std::sync::OnceLock;

use gateway_admin::model::{
    AdminError,
    plugins::{PluginCompatibilityRequirements, PluginHostCompatibility},
};
use gateway_plugin_sdk::{Capability, MANIFEST_VERSION, Manifest, PROTOCOL_VERSION};

const HOST_COMPATIBILITY_JSON: &str = include_str!("../../plugin-host-compatibility.json");

pub(crate) fn host_compatibility() -> Result<&'static PluginHostCompatibility, AdminError> {
    static COMPATIBILITY: OnceLock<Result<PluginHostCompatibility, ()>> = OnceLock::new();
    COMPATIBILITY
        .get_or_init(|| {
            let compatibility =
                serde_json::from_str::<PluginHostCompatibility>(HOST_COMPATIBILITY_JSON)
                    .map_err(|_| ())?;
            // 发行声明可以是 SDK 合同的子集，但不能声称支持当前二进制无法解释的合同
            let known_contracts = compatibility
                .manifest_schema_versions
                .iter()
                .all(|version| *version == MANIFEST_VERSION)
                && compatibility
                    .protocol_versions
                    .iter()
                    .all(|version| *version == PROTOCOL_VERSION)
                && compatibility.capabilities.iter().all(|entry| {
                    serde_json::from_value::<Capability>(entry.capability.clone().into()).is_ok_and(
                        |capability| {
                            entry
                                .versions
                                .iter()
                                .all(|version| capability.contract_versions().contains(version))
                        },
                    )
                });
            (compatibility.is_valid() && known_contracts)
                .then_some(compatibility)
                .ok_or(())
        })
        .as_ref()
        .map_err(|()| AdminError::internal("宿主插件兼容声明不合法"))
}

pub(crate) fn requirements(
    manifest: &Manifest,
) -> Result<PluginCompatibilityRequirements, AdminError> {
    let package = manifest
        .package
        .as_ref()
        .ok_or_else(|| AdminError::invalid("插件包缺少构建元数据"))?;
    Ok(PluginCompatibilityRequirements {
        host_version: manifest.engines.codex_proxy_rs.to_string(),
        manifest_schema_version: manifest.manifest_version,
        protocol_version: package.protocol_version,
        capabilities: manifest
            .contributes
            .iter()
            .map(|(capability, declaration)| {
                (capability.identifier().to_owned(), declaration.version)
            })
            .collect(),
    })
}

pub(crate) fn warning(
    requirements: &PluginCompatibilityRequirements,
    host: &semver::Version,
) -> Result<Option<String>, AdminError> {
    let compatibility = host_compatibility()?;
    let mut warnings = Vec::new();
    if !semver::VersionReq::parse(&requirements.host_version).is_ok_and(|range| range.matches(host))
    {
        warnings.push(format!(
            "插件声明的宿主范围为 {}，当前为 {host}",
            requirements.host_version
        ));
    }
    for (capability, version) in &requirements.capabilities {
        if !compatibility.supports_capability(capability, *version) {
            warnings.push(format!("{capability} v{version} 不在宿主支持范围内"));
        }
    }
    Ok((!warnings.is_empty()).then(|| warnings.join("，")))
}
