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
    assert!(compatibility.supports_capability("middleware", 4));
    assert!(compatibility.supports_capability("upstream_adapter", 1));
    assert!(compatibility.supports_capability("upstream_adapter", 2));
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
    assert_eq!(requirements.capabilities, vec![("middleware".into(), 4)]);
}

#[tokio::test]
async fn deprecated_contracts_are_loadable_and_only_old_versions_receive_notices() {
    for legacy in [false, true] {
        let mut middleware = crate::support::contribution(
            Capability::Middleware,
            vec![Stage::Request],
            vec!["openai".into()],
            vec!["openai".into()],
        );
        let mut upstream = crate::support::contribution(
            Capability::UpstreamAdapter,
            vec![Stage::Upstream],
            vec!["openai".into()],
            vec!["openai".into()],
        );
        if legacy {
            middleware.1.version = 3;
            upstream.1.version = 1;
        }
        let archive = crate::support::package_with_contributions(
            b"not-an-executable",
            Contributions::from([middleware, upstream]),
        );
        let inspector =
            PackageInspector::new(PackageLimits::default(), semver::Version::new(1, 0, 0));
        let artifact = inspector.inspect(archive, None).await.unwrap();
        let notices = inspector.api_deprecations(&artifact.metadata).unwrap();
        assert_eq!(notices.len(), if legacy { 2 } else { 0 });
        for notice in notices {
            assert!(notice.replacement_version > notice.version);
            assert!(!notice.migration.is_empty());
            assert!(notice.remaining_releases <= 7);
        }
    }
}

#[tokio::test]
async fn host_and_capability_version_mismatches_are_warnings_only() {
    let mut declaration = crate::support::contribution(
        Capability::Middleware,
        vec![Stage::Request],
        vec!["openai".into()],
        vec!["openai".into()],
    );
    declaration.1.version = 99;
    let archive =
        crate::support::package_with_contributions(b"fixture", Contributions::from([declaration]));
    let inspector = PackageInspector::new(PackageLimits::default(), semver::Version::new(3, 19, 0));
    let artifact = inspector.inspect(archive.clone(), None).await.unwrap();
    let warning = inspector
        .compatibility_warning(archive, artifact.metadata.sha256)
        .await
        .unwrap()
        .unwrap();
    assert!(warning.contains("middleware v99"));
    assert!(warning.contains("3.19.0"));
}
