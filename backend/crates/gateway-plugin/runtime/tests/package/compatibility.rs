//! 验证宿主兼容性声明与静态制品检查的协议版本约束

use gateway_admin::{
    model::plugins::PluginHostCompatibility, ports::plugins::PluginPackageInspector as _,
};
use gateway_plugin_runtime::{PackageInspector, PackageLimits};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use sha2::{Digest as _, Sha256};

#[test]
fn host_compatibility_only_advertises_known_contracts() {
    let compatibility: PluginHostCompatibility =
        serde_json::from_str(include_str!("../../plugin-host-compatibility.json"))
            .expect("compatibility JSON");
    assert!(compatibility.is_valid());

    assert_eq!(
        compatibility.manifest_schema_versions,
        [gateway_plugin_sdk::MANIFEST_VERSION]
    );
    assert_eq!(
        compatibility.protocol_versions,
        [gateway_plugin_sdk::PROTOCOL_VERSION]
    );
    for entry in &compatibility.capabilities {
        let capability: Capability = serde_json::from_value(entry.capability.clone().into())
            .expect("host capability is understood by the SDK");
        assert_eq!(entry.capability, capability.identifier());
        for version in &entry.versions {
            assert!(
                capability.contract_versions().contains(version),
                "unknown contract {} v{version}",
                entry.capability
            );
        }
    }
}

#[test]
fn host_compatibility_requires_the_trusted_middleware_contract() {
    let compatibility: PluginHostCompatibility =
        serde_json::from_str(include_str!("../../plugin-host-compatibility.json"))
            .expect("compatibility JSON");
    assert!(!compatibility.supports_capability("middleware", 1));
    assert!(!compatibility.supports_capability("middleware", 2));
    assert!(compatibility.supports_capability("middleware", 3));
    assert!(!compatibility.supports_capability("openai", 1));
}

#[tokio::test]
async fn package_inspector_returns_static_requirements_without_starting_the_plugin() {
    let archive = crate::support::package_with_contributions(
        b"not-an-executable",
        Contributions::from([crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        )]),
    );
    let digest = hex::encode(Sha256::digest(archive.as_ref()));
    let inspector = PackageInspector::new(PackageLimits::default(), semver::Version::new(3, 13, 0));

    let requirements = inspector
        .compatibility(archive, digest)
        .await
        .expect("compatibility requirements");
    assert_eq!(requirements.host_version, ">=1.0.0, <2.0.0");
    assert_eq!(
        requirements.manifest_schema_version,
        gateway_plugin_sdk::MANIFEST_VERSION
    );
    assert_eq!(
        requirements.protocol_version,
        gateway_plugin_sdk::PROTOCOL_VERSION
    );
    assert_eq!(requirements.capabilities, vec![("middleware".into(), 3)]);
}
