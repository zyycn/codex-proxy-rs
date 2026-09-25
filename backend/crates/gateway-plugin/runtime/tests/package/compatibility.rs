use gateway_admin::{
    model::plugins::PluginHostCompatibility, ports::plugins::PluginPackageInspector as _,
};
use gateway_plugin_runtime::{PackageInspector, PackageLimits};
use gateway_plugin_sdk::{Capability, Contributions, Permission, Stage};
use sha2::{Digest as _, Sha256};

#[test]
fn host_compatibility_covers_every_sdk_capability_and_permission() {
    let compatibility: PluginHostCompatibility =
        serde_json::from_str(include_str!("../../plugin-host-compatibility.json"))
            .expect("compatibility JSON");
    assert!(compatibility.is_valid());

    for capability in all_capabilities() {
        assert_exhaustive_capability(capability);
        let identifier = identifier(capability);
        assert!(
            compatibility.supports_capability(&identifier, 1),
            "missing capability {identifier}"
        );
    }
    for permission in Permission::ALL {
        assert_exhaustive_permission(permission);
        let identifier = identifier(permission);
        assert!(
            compatibility.supports_permission(&identifier),
            "missing permission {identifier}"
        );
    }
}

#[tokio::test]
async fn package_inspector_returns_static_requirements_without_starting_the_plugin() {
    let archive = crate::support::package_with_contributions(
        b"not-an-executable",
        vec![],
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
    assert_eq!(requirements.capabilities, vec![("middleware".into(), 1)]);
    assert!(requirements.permissions.is_empty());
}

fn identifier(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .expect("serialize identifier")
        .as_str()
        .expect("string identifier")
        .to_owned()
}

fn assert_exhaustive_capability(capability: Capability) {
    match capability {
        Capability::FrontendAuthentication
        | Capability::Scheduler
        | Capability::ModelRouter
        | Capability::ModelCatalog
        | Capability::RetryPolicy
        | Capability::Middleware
        | Capability::RequestLifecycle
        | Capability::WebSocketObserver
        | Capability::Usage
        | Capability::CommandLine
        | Capability::Management => {}
    }
}

fn assert_exhaustive_permission(permission: Permission) {
    match permission {
        Permission::Network
        | Permission::Requests
        | Permission::Models
        | Permission::Accounts
        | Permission::Data
        | Permission::PublicEndpoints => {}
    }
}

fn all_capabilities() -> [Capability; 11] {
    [
        Capability::FrontendAuthentication,
        Capability::Scheduler,
        Capability::ModelRouter,
        Capability::ModelCatalog,
        Capability::RetryPolicy,
        Capability::Middleware,
        Capability::RequestLifecycle,
        Capability::WebSocketObserver,
        Capability::Usage,
        Capability::CommandLine,
        Capability::Management,
    ]
}
