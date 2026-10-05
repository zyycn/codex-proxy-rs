//! 插件分发来源、凭据与远程制品安装的用例测试

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    PluginDistributionPorts, PluginsService,
    model::{
        AdminError, AdminErrorKind, MutationActor, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMetadata,
            PluginArtifactMutation, PluginSource, PluginSourceEgress,
            distribution::{
                DownloadedPlugin, GithubReleaseQuery, PluginDistributionEgress, PluginRelease,
                PluginSourceBinding, PluginUpdatePolicy, PluginUpdateSource, RemotePluginInstall,
                RemotePluginLocation, RemotePluginVerify, SourceCredential, SourceCredentialInfo,
            },
            instances::{PluginInstance, PluginInstanceMutation, PluginInstanceSnapshot},
        },
        proxies::{
            NewProxy, ProxyAccountListQuery, ProxyAccountPage, ProxyListQuery, ProxyMutation,
            ProxyPage, ProxyRecord, ProxyTestResult, UpdateProxy,
        },
    },
    ports::{
        plugins::{PluginDistribution, PluginPackageInspector, PluginStore},
        proxy::{ProxyImportReservation, ProxyStore},
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use gateway_core::{
    account::{OutboundProxy, ProviderAccountId},
    routing::ConfigRevision,
    runtime::{RuntimeSnapshotHandle, SnapshotControl},
};

use super::TestPluginPorts;

struct Fixture {
    binding: Mutex<PluginSourceBinding>,
    queries: Mutex<Vec<GithubReleaseQuery>>,
    publications: Mutex<Vec<u64>>,
    changes: Mutex<usize>,
    query_fails: bool,
    change_during_query: bool,
    package: Option<InspectedPluginArtifact>,
    downloads: Mutex<usize>,
    change_during_download: bool,
    proxy: Mutex<Option<ProxyRecord>>,
    egresses: Mutex<Vec<Option<PluginSourceEgress>>>,
    change_proxy_during_download: bool,
}

impl Fixture {
    fn new(policy: PluginUpdatePolicy) -> Self {
        Self {
            binding: Mutex::new(PluginSourceBinding {
                plugin_id: "test.example".into(),
                source: PluginUpdateSource::Github {
                    repository: "owner/plugin".into(),
                },
                policy,
                outbound_proxy_id: None,
            }),
            queries: Mutex::new(vec![]),
            publications: Mutex::new(vec![]),
            changes: Mutex::new(0),
            query_fails: false,
            change_during_query: false,
            package: None,
            downloads: Mutex::new(0),
            change_during_download: false,
            proxy: Mutex::new(None),
            egresses: Mutex::new(vec![]),
            change_proxy_during_download: false,
        }
    }

    fn service(self: &Arc<Self>) -> PluginsService {
        let unused = Arc::new(TestPluginPorts);
        PluginsService::new(
            self.clone(),
            self.clone(),
            PluginDistributionPorts::new(self.clone(), self.clone()),
            self.clone(),
            unused.clone(),
            RuntimeSnapshotHandle::default(),
            unused,
        )
    }
}

impl SnapshotControl for Fixture {
    fn publish_committed(&self, revision: ConfigRevision) -> BoxFuture<'_, ()> {
        self.publications.lock().unwrap().push(revision.get());
        Box::pin(async {})
    }
}

#[async_trait]
impl ProxyStore for Fixture {
    async fn reserve_import(&self, _: &str) -> AdminStoreResult<ProxyImportReservation> {
        unreachable!()
    }

    async fn list(&self, _: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        unreachable!()
    }

    async fn list_accounts(&self, _: ProxyAccountListQuery) -> AdminStoreResult<ProxyAccountPage> {
        unreachable!()
    }

    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord> {
        self.proxy
            .lock()
            .unwrap()
            .as_ref()
            .filter(|record| record.id == id)
            .cloned()
            .ok_or_else(|| {
                AdminStoreError::new(AdminStoreErrorKind::NotFound, "proxy", "fixture not found")
            })
    }

    async fn remove_account(
        &self,
        _: &str,
        _: &ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        unreachable!()
    }

    async fn create(&self, _: NewProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        unreachable!()
    }

    async fn update(&self, _: UpdateProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        unreachable!()
    }

    async fn delete(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        unreachable!()
    }

    async fn record_test(
        &self,
        _: &str,
        _: Revision,
        _: ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        unreachable!()
    }
}

#[async_trait]
impl PluginDistribution for Fixture {
    fn validate_source(&self, _: &PluginUpdateSource) -> Result<(), AdminError> {
        Ok(())
    }
    fn validate_credential(&self, _: &SourceCredential) -> Result<(), AdminError> {
        unreachable!()
    }
    async fn query_release(
        &self,
        query: GithubReleaseQuery,
        credentials: Vec<SourceCredential>,
        egress: Option<PluginDistributionEgress>,
    ) -> Result<PluginRelease, AdminError> {
        assert!(credentials.is_empty());
        self.egresses
            .lock()
            .unwrap()
            .push(egress.map(|egress| egress.source));
        self.queries.lock().unwrap().push(query.clone());
        if self.query_fails {
            return Err(AdminError::unavailable("fixture rate limited"));
        }
        if self.change_during_query {
            self.binding.lock().unwrap().policy = PluginUpdatePolicy::Manual {};
        }
        Ok(PluginRelease {
            repository: query.repository,
            tag: query.tag.unwrap_or_else(|| "v2.0.0".into()),
            name: "fixture".into(),
            prerelease: query.allow_prerelease,
            assets: vec![],
            queried_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
        })
    }
    async fn download(
        &self,
        location: RemotePluginLocation,
        _: Vec<SourceCredential>,
        egress: Option<PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError> {
        let outbound_proxy = egress.as_ref().map(|egress| egress.source.clone());
        self.egresses.lock().unwrap().push(outbound_proxy.clone());
        let package = self.package.as_ref().expect("检查更新不能下载产物");
        *self.downloads.lock().unwrap() += 1;
        if self.change_during_download {
            self.binding.lock().unwrap().policy = PluginUpdatePolicy::Manual {};
        }
        if self.change_proxy_during_download {
            self.proxy.lock().unwrap().as_mut().unwrap().revision = Revision::new(2).unwrap();
        }
        let RemotePluginLocation::Github {
            repository,
            tag,
            asset,
            ..
        } = location
        else {
            unreachable!()
        };
        Ok(DownloadedPlugin {
            archive: package.archive.clone(),
            sha256: package.metadata.sha256.clone(),
            source: PluginSource::Github {
                repository: repository.to_ascii_lowercase(),
                tag,
                asset,
                credential_ids: vec![],
                outbound_proxy,
            },
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
        let package = self.package.as_ref().expect("检查 Release 不能校验插件包");
        assert_eq!(archive, package.archive);
        if let Some(digest) = expected_sha256 {
            assert_eq!(digest, package.metadata.sha256);
        }
        Ok(package.clone())
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
    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        Ok(vec![self.binding.lock().unwrap().clone()])
    }
    async fn change_update_source(
        &self,
        binding: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        *self.binding.lock().unwrap() = binding;
        *self.changes.lock().unwrap() += 1;
        Ok(Revision::new(2).unwrap())
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
    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        unreachable!()
    }
    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        Err(super::unavailable())
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
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        panic!("检查更新不能安装产物")
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

fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "plugin-update-test".into(),
    }
}

#[tokio::test]
async fn update_check_uses_persisted_policy_without_downloading_writing_or_publishing() {
    for (policy, tag, prerelease) in [
        (PluginUpdatePolicy::Stable {}, None, false),
        (
            PluginUpdatePolicy::Pinned {
                tag: "v2.0.0-rc.1".into(),
                allow_prerelease: true,
            },
            Some("v2.0.0-rc.1"),
            true,
        ),
    ] {
        let fixture = Arc::new(Fixture::new(policy.clone()));
        let check = fixture
            .service()
            .check_update("test.example", &[])
            .await
            .unwrap();
        assert_eq!(check.binding.policy, policy);
        assert_eq!(
            fixture.queries.lock().unwrap().as_slice(),
            &[GithubReleaseQuery {
                repository: "owner/plugin".into(),
                tag: tag.map(str::to_owned),
                allow_prerelease: prerelease,
            }]
        );
        assert_eq!(*fixture.changes.lock().unwrap(), 0);
        assert!(fixture.publications.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn update_check_never_turns_query_failure_or_changed_source_into_no_updates() {
    for changed in [false, true] {
        let mut fixture = Fixture::new(PluginUpdatePolicy::Stable {});
        fixture.query_fails = !changed;
        fixture.change_during_query = changed;
        let fixture = Arc::new(fixture);
        let error = fixture
            .service()
            .check_update("test.example", &[])
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            if changed {
                AdminErrorKind::Conflict
            } else {
                AdminErrorKind::Unavailable
            }
        );
        assert!(fixture.publications.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn update_check_rejects_manual_missing_and_duplicate_credential_selections_before_query() {
    let fixture = Arc::new(Fixture::new(PluginUpdatePolicy::Manual {}));
    assert_eq!(
        fixture
            .service()
            .check_update("test.example", &[])
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::Invalid
    );
    assert_eq!(
        fixture
            .service()
            .check_update("missing", &[])
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::NotFound
    );
    fixture.binding.lock().unwrap().policy = PluginUpdatePolicy::Stable {};
    assert_eq!(
        fixture
            .service()
            .check_update("test.example", &["same".into(), "same".into()])
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::Invalid
    );
    assert!(fixture.queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn changing_source_validates_policy_and_publishes_only_explicit_valid_changes() {
    let fixture = Arc::new(Fixture::new(PluginUpdatePolicy::Manual {}));
    let original = fixture.binding.lock().unwrap().clone();
    for (source, policy) in [
        (PluginUpdateSource::Upload, PluginUpdatePolicy::Stable {}),
        (PluginUpdateSource::Builtin, PluginUpdatePolicy::Manual {}),
        (
            original.source.clone(),
            PluginUpdatePolicy::Pinned {
                tag: " ".into(),
                allow_prerelease: false,
            },
        ),
    ] {
        let error = fixture
            .service()
            .change_update_source(
                PluginSourceBinding {
                    plugin_id: "test.example".into(),
                    source,
                    policy,
                    outbound_proxy_id: None,
                },
                &context(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), AdminErrorKind::Invalid);
    }
    assert_eq!(*fixture.changes.lock().unwrap(), 0);
    let binding = PluginSourceBinding {
        plugin_id: "test.example".into(),
        source: PluginUpdateSource::Github {
            repository: "Owner/Plugin".into(),
        },
        policy: PluginUpdatePolicy::Stable {},
        outbound_proxy_id: None,
    };
    fixture
        .service()
        .change_update_source(binding, &context())
        .await
        .unwrap();
    assert_eq!(fixture.binding.lock().unwrap().source, original.source);
    assert_eq!(*fixture.changes.lock().unwrap(), 1);
    assert_eq!(fixture.publications.lock().unwrap().as_slice(), &[2]);
}

#[test]
fn existing_source_payload_defaults_to_manual_and_rejects_unknown_policy_fields() {
    let binding: PluginSourceBinding = serde_json::from_value(
        serde_json::json!({"pluginId":"test.example","source":{"kind":"upload"}}),
    )
    .unwrap();
    assert_eq!(binding.policy, PluginUpdatePolicy::Manual {});
    assert!(serde_json::from_value::<PluginSourceBinding>(serde_json::json!({"pluginId":"test.example","source":{"kind":"github","repository":"owner/plugin"},"policy":{"kind":"stable","autoInstall":true}})).is_err());
}

fn preview_fixture() -> Fixture {
    let mut fixture = Fixture::new(PluginUpdatePolicy::Stable {});
    fixture.package = Some(InspectedPluginArtifact {
        metadata: PluginArtifactMetadata {
            plugin_id: "test.example".into(),
            version: "2.0.0".into(),
            name: "example".into(),
            display_name: "Example".into(),
            publisher: "test".into(),
            author: Some("Tests".into()),
            description: String::new(),
            license: "MIT".into(),
            sha256: "a".repeat(64),
            platforms: vec!["linux-x86_64".into()],
            icon: None,
            contributes: Default::default(),

            configuration_schema: serde_json::json!({"type":"object"}),
            secret_fields: vec![],
            state_namespaces: vec![],
        },
        archive: b"bounded archive fixture".as_slice().into(),
    });
    fixture
}

fn source_proxy() -> ProxyRecord {
    let now = chrono::Utc::now();
    ProxyRecord {
        auto_location: false,
        detected_location: None,
        location: None,
        id: "source-proxy".into(),
        name: "Source proxy".into(),
        proxy: OutboundProxy::parse("http://user:secret@127.0.0.1:18080").unwrap(),
        revision: Revision::new(1).unwrap(),
        account_count: 0,
        last_test_at: None,
        last_test: None,
        created_at: now,
        updated_at: now,
    }
}

fn preview_request() -> RemotePluginVerify {
    RemotePluginVerify {
        expected_plugin_id: Some("test.example".into()),
        credential_ids: vec![],
        outbound_proxy_id: None,
        location: RemotePluginLocation::Github {
            repository: "Owner/Plugin".into(),
            tag: "v2.0.0".into(),
            asset: "plugin.tar.gz".into(),
            allow_prerelease: false,
            sha256: Some("a".repeat(64)),
        },
    }
}

fn install_request() -> RemotePluginInstall {
    let preview = preview_request();
    RemotePluginInstall {
        plugin_id: "test.example".into(),
        version: "2.0.0".into(),
        credential_ids: preview.credential_ids,
        outbound_proxy_id: preview.outbound_proxy_id,
        location: preview.location,
    }
}

#[tokio::test]
async fn saved_source_proxy_is_resolved_for_queries_and_frozen_into_verified_source() {
    let mut fixture = preview_fixture();
    fixture.binding.get_mut().unwrap().outbound_proxy_id = Some("source-proxy".into());
    *fixture.proxy.get_mut().unwrap() = Some(source_proxy());
    let fixture = Arc::new(fixture);
    fixture
        .service()
        .check_update("test.example", &[])
        .await
        .unwrap();
    let mut request = preview_request();
    request.outbound_proxy_id = Some("source-proxy".into());
    let verified = fixture.service().verify_remote(request).await.unwrap();
    assert!(matches!(
        verified.source.outbound_proxy(),
        Some(PluginSourceEgress { id, revision: 1 }) if id == "source-proxy"
    ));
    assert_eq!(
        fixture.egresses.lock().unwrap().as_slice(),
        &[
            Some(PluginSourceEgress {
                id: "source-proxy".into(),
                revision: 1,
            }),
            Some(PluginSourceEgress {
                id: "source-proxy".into(),
                revision: 1,
            }),
        ]
    );
}

#[tokio::test]
async fn proxy_revision_change_during_download_rejects_the_verified_candidate() {
    let mut fixture = preview_fixture();
    fixture.binding.get_mut().unwrap().outbound_proxy_id = Some("source-proxy".into());
    *fixture.proxy.get_mut().unwrap() = Some(source_proxy());
    fixture.change_proxy_during_download = true;
    let fixture = Arc::new(fixture);
    let mut request = preview_request();
    request.outbound_proxy_id = Some("source-proxy".into());
    assert_eq!(
        fixture
            .service()
            .verify_remote(request)
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::Conflict
    );
}

#[tokio::test]
async fn verifying_remote_package_uses_inspector_and_returns_fixed_facts_without_installing() {
    let fixture = Arc::new(preview_fixture());
    let verified = fixture
        .service()
        .verify_remote(preview_request())
        .await
        .unwrap();
    assert_eq!(verified.metadata.sha256, "a".repeat(64));
    assert!(
        matches!(verified.source, PluginSource::Github { ref repository, ref tag, ref asset, .. }
        if repository == "owner/plugin" && tag == "v2.0.0" && asset == "plugin.tar.gz")
    );
    assert_eq!(*fixture.downloads.lock().unwrap(), 1);
    assert_eq!(*fixture.changes.lock().unwrap(), 0);
    assert!(fixture.publications.lock().unwrap().is_empty());
}

#[tokio::test]
async fn remote_install_rejects_non_namespaced_plugin_ids_before_download() {
    for plugin_id in [
        "example",
        "Test.example",
        "test.-example",
        "test.example_legacy",
        "test.example.extra",
    ] {
        let fixture = Arc::new(preview_fixture());
        let mut request = preview_request();
        request.expected_plugin_id = Some(plugin_id.into());
        let error = fixture.service().verify_remote(request).await.unwrap_err();
        assert_eq!(error.kind(), AdminErrorKind::Invalid, "{plugin_id}");
        assert_eq!(*fixture.downloads.lock().unwrap(), 0, "{plugin_id}");
    }
}

#[tokio::test]
async fn installation_rejects_identity_version_and_source_changes_without_persisting() {
    for change in 0..4 {
        let mut fixture = preview_fixture();
        match change {
            0 => fixture.package.as_mut().unwrap().metadata.plugin_id = "foreign".into(),
            1 => fixture.package.as_mut().unwrap().metadata.version = "3.0.0".into(),
            2 => fixture.binding.lock().unwrap().source = PluginUpdateSource::Upload,
            _ => fixture.change_during_download = true,
        }
        let fixture = Arc::new(fixture);
        let error = fixture
            .service()
            .install_remote(install_request(), &context())
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.kind(),
            if change < 2 {
                AdminErrorKind::Invalid
            } else {
                AdminErrorKind::Conflict
            }
        );
        assert_eq!(*fixture.downloads.lock().unwrap(), usize::from(change != 2));
        assert!(fixture.publications.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn preview_discovers_identity_and_version_from_manifest_without_installing() {
    let fixture = Arc::new(preview_fixture());
    let mut request = preview_request();
    request.expected_plugin_id = None;
    let RemotePluginLocation::Github { sha256, .. } = &mut request.location else {
        unreachable!()
    };
    *sha256 = None;
    let verified = fixture.service().verify_remote(request).await.unwrap();
    assert_eq!(verified.metadata.plugin_id, "test.example");
    assert_eq!(verified.metadata.version, "2.0.0");
    assert!(fixture.publications.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discovered_identity_cannot_bypass_saved_source_or_concurrent_changes() {
    for changed_during_download in [false, true] {
        let mut fixture = preview_fixture();
        if changed_during_download {
            fixture.change_during_download = true;
        } else {
            fixture.binding.get_mut().unwrap().source = PluginUpdateSource::Upload;
        }
        let fixture = Arc::new(fixture);
        let mut request = preview_request();
        request.expected_plugin_id = None;
        let error = fixture.service().verify_remote(request).await.unwrap_err();
        assert_eq!(error.kind(), AdminErrorKind::Conflict);
        assert!(fixture.publications.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn installation_requires_pinned_digest_before_downloading() {
    let fixture = Arc::new(preview_fixture());
    let mut request = install_request();
    let RemotePluginLocation::Github { sha256, .. } = &mut request.location else {
        unreachable!()
    };
    *sha256 = None;
    let error = fixture
        .service()
        .install_remote(request, &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
    assert_eq!(*fixture.downloads.lock().unwrap(), 0);
}

#[tokio::test]
async fn upload_preview_inspects_without_persisting_or_publishing() {
    let fixture = Arc::new(preview_fixture());
    let archive = fixture.package.as_ref().unwrap().archive.clone();
    let verified = fixture.service().verify_upload(archive).await.unwrap();
    assert_eq!(verified.metadata.plugin_id, "test.example");
    assert_eq!(verified.source, PluginSource::Upload);
    assert!(fixture.publications.lock().unwrap().is_empty());
}

#[test]
fn url_preview_accepts_only_a_location_without_identity_or_digest() {
    let input: RemotePluginVerify = serde_json::from_value(serde_json::json!({
        "location": {"kind": "url", "url": "https://example.org/plugin.tar.gz"}
    }))
    .unwrap();
    assert!(input.expected_plugin_id.is_none());
    assert!(matches!(
        input.location,
        RemotePluginLocation::Url { sha256: None, .. }
    ));
}
