//! 官方插件发行清单校验与平台制品导入的用例测试

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    PluginDistributionPorts, PluginsService,
    model::{
        AdminError, AdminErrorKind, MutationActor, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMetadata,
            PluginArtifactMutation, PluginSource,
            distribution::{PluginSourceBinding, SourceCredential, SourceCredentialInfo},
            instances::{PluginInstance, PluginInstanceMutation, PluginInstanceSnapshot},
            official::OfficialPluginReleaseIdentity,
        },
    },
    ports::{
        plugin_release::{
            OfficialPluginReleaseFiles, OfficialPluginReleaseReadError,
            OfficialPluginReleaseReadErrorKind,
        },
        plugins::{PluginPackageInspector, PluginStore},
        store::AdminStoreResult,
    },
};
use gateway_core::{
    routing::ConfigRevision,
    runtime::{RuntimeSnapshotHandle, SnapshotControl},
};

use super::TestPluginPorts;

const VERSION: &str = "3.13.0";
const GIT_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Fixture {
    manifest: Option<Arc<[u8]>>,
    archives: BTreeMap<String, Arc<[u8]>>,
    inspected_publisher: String,
    installs: std::sync::Mutex<Vec<(String, PluginSource)>>,
    publications: std::sync::Mutex<Vec<u64>>,
    fail_archive: Option<Vec<u8>>,
}

impl Fixture {
    fn new(plugins: &[(&str, u8)]) -> Arc<Self> {
        let artifacts = plugins
            .iter()
            .map(|(id, marker)| plugin_entry(id, *marker))
            .collect::<Vec<_>>();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "sealed": true,
            "gateway_version": VERSION,
            "gateway_git_sha": GIT_SHA,
            "plugin_host": plugin_host(),
            "plugins": artifacts,
        }))
        .expect("manifest");
        let archives = plugins
            .iter()
            .map(|(id, marker)| {
                (
                    format!(
                        "{id}-1.0.0-{}-{}.tar.gz",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ),
                    Arc::<[u8]>::from(vec![*marker]),
                )
            })
            .collect();
        Arc::new(Self {
            manifest: Some(manifest.into()),
            archives,
            inspected_publisher: "test".into(),
            installs: std::sync::Mutex::new(Vec::new()),
            publications: std::sync::Mutex::new(Vec::new()),
            fail_archive: None,
        })
    }

    fn service(self: &Arc<Self>) -> PluginsService {
        let unused = Arc::new(TestPluginPorts);
        PluginsService::new(
            self.clone(),
            self.clone(),
            PluginDistributionPorts::new(unused.clone(), unused.clone()),
            self.clone(),
            unused.clone(),
            RuntimeSnapshotHandle::default(),
            unused,
        )
    }
}

#[async_trait]
impl OfficialPluginReleaseFiles for Fixture {
    async fn manifest(&self) -> Result<Option<Arc<[u8]>>, OfficialPluginReleaseReadError> {
        Ok(self.manifest.clone())
    }

    async fn artifact(&self, file_name: &str) -> Result<Arc<[u8]>, OfficialPluginReleaseReadError> {
        self.archives.get(file_name).cloned().ok_or_else(|| {
            OfficialPluginReleaseReadError::new(OfficialPluginReleaseReadErrorKind::NotFound)
        })
    }
}

#[async_trait]
impl PluginPackageInspector for Fixture {
    async fn inspect(
        &self,
        archive: Arc<[u8]>,
        expected_sha256: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError> {
        if self
            .fail_archive
            .as_ref()
            .is_some_and(|expected| archive.as_ref() == expected)
        {
            return Err(AdminError::invalid("fixture rejected package"));
        }
        let marker = archive.first().copied().expect("marker");
        let name = format!("official-{marker}");
        let id = format!("test.{name}");
        Ok(InspectedPluginArtifact {
            metadata: PluginArtifactMetadata {
                plugin_id: id,
                version: "1.0.0".into(),
                name,
                display_name: "Official fixture".into(),
                publisher: self.inspected_publisher.clone(),
                author: Some("project".into()),
                description: String::new(),
                license: "MIT".into(),
                sha256: expected_sha256.expect("expected digest"),
                platforms: vec![format!(
                    "{}-{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )],
                icon: None,
                contributes: Default::default(),

                configuration_schema: serde_json::json!({}),
                secret_fields: Vec::new(),
                state_namespaces: Vec::new(),
            },
            archive,
        })
    }
}

impl SnapshotControl for Fixture {
    fn publish_committed(&self, revision: ConfigRevision) -> BoxFuture<'_, ()> {
        self.publications.lock().unwrap().push(revision.get());
        Box::pin(async {})
    }
}

#[async_trait]
impl PluginStore for Fixture {
    async fn management_target_is_current(
        &self,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> AdminStoreResult<bool> {
        Ok(self
            .load_instances()
            .await?
            .instances
            .iter()
            .any(|instance| {
                instance.enabled
                    && instance.trusted_process
                    && instance.id == target.instance_id
                    && instance.artifact_sha256 == target.artifact_sha256
                    && instance.revision.get() == target.revision
            }))
    }
    async fn load_instances(&self) -> AdminStoreResult<PluginInstanceSnapshot> {
        unreachable!()
    }
    async fn save_instance(
        &self,
        _: PluginInstance,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        unreachable!()
    }
    async fn delete_instance(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        unreachable!()
    }
    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        unreachable!()
    }
    async fn change_update_source(
        &self,
        _: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        unreachable!()
    }
    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        unreachable!()
    }
    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        unreachable!()
    }
    async fn save_source_credential(
        &self,
        _: SourceCredential,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        unreachable!()
    }
    async fn delete_source_credential(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        unreachable!()
    }
    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        unreachable!()
    }
    async fn load_artifact(&self, _: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        unreachable!()
    }
    async fn install_artifact(
        &self,
        artifact: InspectedPluginArtifact,
        source: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        self.installs
            .lock()
            .unwrap()
            .push((artifact.metadata.plugin_id.clone(), source.clone()));
        Ok(PluginArtifactMutation {
            config_revision: Revision::new(2).unwrap(),
            artifact: InstalledPluginArtifact {
                metadata: artifact.metadata,
                source,
                installed_at: chrono::Utc::now(),
                accepted_at: None,
            },
        })
    }
    async fn accept_artifact(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        unreachable!()
    }
    async fn delete_artifact(&self, _: &str, _: &MutationContext) -> AdminStoreResult<Revision> {
        unreachable!()
    }
}

#[tokio::test]
async fn official_release_validates_all_packages_then_uses_builtin_install_path() {
    let fixture = Fixture::new(&[("test.official-1", 1)]);
    let result = fixture
        .service()
        .import_official_release(fixture.as_ref(), &identity(), &system_context())
        .await
        .expect("official import");

    assert_eq!(result.artifacts, 1);
    assert_eq!(result.config_revision.map(Revision::get), Some(2));
    assert_eq!(
        fixture.installs.lock().unwrap().as_slice(),
        &[(
            "test.official-1".into(),
            PluginSource::Builtin {
                release: VERSION.into()
            }
        )]
    );
    assert_eq!(fixture.publications.lock().unwrap().as_slice(), &[2]);
}

#[tokio::test]
async fn official_release_accepts_legacy_update_envelope_with_current_plugin_contracts() {
    let mut fixture = Fixture::new(&[]);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(fixture.manifest.as_ref().unwrap()).unwrap();
    manifest["plugin_host"]["schema_version"] = serde_json::json!(1);
    manifest["plugin_host"]["permissions"] = serde_json::json!([]);
    Arc::get_mut(&mut fixture).unwrap().manifest =
        Some(serde_json::to_vec(&manifest).unwrap().into());
    let imported = fixture
        .service()
        .import_official_release(fixture.as_ref(), &identity(), &system_context())
        .await
        .unwrap();
    assert_eq!(imported.artifacts, 0);
}

#[tokio::test]
async fn official_release_still_rejects_an_unknown_host_contract_at_startup() {
    let mut fixture = Fixture::new(&[]);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(fixture.manifest.as_ref().unwrap()).unwrap();
    manifest["plugin_host"]["schema_version"] = serde_json::json!(99);
    Arc::get_mut(&mut fixture).unwrap().manifest =
        Some(serde_json::to_vec(&manifest).unwrap().into());
    assert!(
        fixture
            .service()
            .import_official_release(fixture.as_ref(), &identity(), &system_context())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn official_release_never_persists_a_valid_prefix_when_later_validation_fails() {
    let mut fixture = Fixture::new(&[("test.official-1", 1), ("test.official-2", 2)]);
    Arc::get_mut(&mut fixture).unwrap().fail_archive = Some(vec![2]);
    let error = fixture
        .service()
        .import_official_release(fixture.as_ref(), &identity(), &system_context())
        .await
        .expect_err("second package must fail");

    assert_eq!(error.kind(), AdminErrorKind::Invalid);
    assert!(fixture.installs.lock().unwrap().is_empty());
    assert!(fixture.publications.lock().unwrap().is_empty());
}

#[tokio::test]
async fn official_release_rejects_publisher_identity_mismatches() {
    let fixture = Fixture::new(&[("other.official-1", 1)]);
    assert_eq!(
        fixture
            .service()
            .import_official_release(fixture.as_ref(), &identity(), &system_context())
            .await
            .expect_err("publisher must match the plugin ID namespace")
            .kind(),
        AdminErrorKind::Invalid
    );
    assert!(fixture.installs.lock().unwrap().is_empty());

    let mut fixture = Fixture::new(&[("test.official-1", 1)]);
    Arc::get_mut(&mut fixture).unwrap().inspected_publisher = "other".into();
    assert_eq!(
        fixture
            .service()
            .import_official_release(fixture.as_ref(), &identity(), &system_context())
            .await
            .expect_err("inspected publisher must match the sealed release")
            .kind(),
        AdminErrorKind::Invalid
    );
    assert!(fixture.installs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn official_release_rejects_non_system_callers_and_mismatched_build_identity() {
    let fixture = Fixture::new(&[("test.official-1", 1)]);
    let admin = MutationContext {
        actor: MutationActor::AdminApiKey,
        request_id: "fixture".into(),
    };
    assert_eq!(
        fixture
            .service()
            .import_official_release(fixture.as_ref(), &identity(), &admin)
            .await
            .expect_err("admin caller")
            .kind(),
        AdminErrorKind::Forbidden
    );
    let wrong = OfficialPluginReleaseIdentity {
        gateway_version: "9.9.9".into(),
        gateway_git_sha: GIT_SHA.into(),
    };
    assert_eq!(
        fixture
            .service()
            .import_official_release(fixture.as_ref(), &wrong, &system_context())
            .await
            .expect_err("mismatched identity")
            .kind(),
        AdminErrorKind::Invalid
    );
    assert!(fixture.installs.lock().unwrap().is_empty());
}

fn identity() -> OfficialPluginReleaseIdentity {
    OfficialPluginReleaseIdentity {
        gateway_version: VERSION.into(),
        gateway_git_sha: GIT_SHA.into(),
    }
}

fn plugin_host() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 2,
        "manifest_schema_versions": [2],
        "protocol_versions": [3],
        "capabilities": [{ "capability": "executor", "versions": [1] }],
    })
}

fn system_context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "official-import-fixture".into(),
    }
}

fn plugin_entry(id: &str, marker: u8) -> serde_json::Value {
    let (release_os, release_architecture, target) = release_platform();
    serde_json::json!({
        "id": id,
        "version": "1.0.0",
        "publisher": "test",
        "stability": "experimental",
        "artifacts": [{
            "release_os": release_os,
            "release_architecture": release_architecture,
            "target": target,
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "archive": format!("{id}-1.0.0-{}-{}.tar.gz", std::env::consts::OS, std::env::consts::ARCH),
            "sha256": format!("{marker:01x}").repeat(64),
        }]
    })
}

fn release_platform() -> (&'static str, &'static str, &'static str) {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => ("linux", "amd64", "x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => ("linux", "arm64", "aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => ("darwin", "arm64", "aarch64-apple-darwin"),
        platform => panic!("unsupported test platform: {platform:?}"),
    }
}
