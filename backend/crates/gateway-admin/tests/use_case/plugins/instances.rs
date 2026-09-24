use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    PluginDistributionPorts, PluginsService,
    model::{
        AdminError, AdminErrorKind, MutationActor, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMetadata,
            PluginArtifactMutation, PluginContribution, PluginSource,
            distribution::{PluginSourceBinding, SourceCredential, SourceCredentialInfo},
            instances::{
                ConfigurePluginInstance, PluginCapabilityBinding, PluginFailurePolicy,
                PluginInstance, PluginInstanceMutation, PluginInstanceReplacement,
                PluginInstanceRuntime, PluginInstanceRuntimeFailure, PluginInstanceRuntimeStatus,
                PluginInstanceSnapshot, PluginPermissionGrant, PluginVersionConfiguration,
                RollbackPluginInstance,
            },
            state::{
                ApplyPluginStateMigration, DeletePluginState, PluginStateCommit,
                PluginStateConfiguration, PluginStateMigrationBatch, PluginStateMigrationNamespace,
                PluginStateOwner, PluginStateOwnerRequest, PluginStateRecord, PluginStateSchema,
                PluginStateTransition, PluginStateWrite, PutPluginState,
            },
        },
    },
    ports::{
        plugins::{
            PluginPreparation, PluginStateStore, PluginStateStoreError, PluginStateStoreErrorKind,
            PluginStateStoreResult, PluginStore,
        },
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use gateway_core::{
    routing::ConfigRevision,
    runtime::{
        RuntimeSnapshotHandle, SnapshotControl,
        extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
    },
};
use serde_json::json;

use super::TestPluginPorts;

const OLD_ARTIFACT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW_ARTIFACT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[derive(Clone, Copy)]
enum MigrationBehavior {
    Succeed,
    Fail,
    ConcurrentChangeThenFail,
}

struct SavedInstance {
    artifact_sha256: String,
    enabled: bool,
    transition_id: Option<String>,
}

struct FixtureData {
    snapshot: PluginInstanceSnapshot,
    artifacts: Vec<InstalledPluginArtifact>,
    artifact_reads: usize,
    change_during_artifact_read: bool,
    fail_preparation: bool,
    configuration_ready: bool,
    prepared_snapshots: Vec<PluginInstanceSnapshot>,
    saves: Vec<SavedInstance>,
    history: BTreeMap<String, PluginVersionConfiguration>,
    aborted: usize,
    activated: usize,
    quiesced: usize,
    diagnostics: Option<BTreeMap<String, PluginInstanceRuntime>>,
}

struct LifecycleFixture {
    behavior: MigrationBehavior,
    data: Mutex<FixtureData>,
    next_prepared: AtomicU64,
}

impl LifecycleFixture {
    fn new(behavior: MigrationBehavior) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            data: Mutex::new(FixtureData {
                snapshot: PluginInstanceSnapshot {
                    config_revision: revision(10),
                    instances: vec![PluginInstance {
                        id: "00000000-0000-7000-8000-000000000001".into(),
                        name: "previous".into(),
                        artifact_sha256: OLD_ARTIFACT.into(),
                        enabled: true,
                        trusted_process: true,
                        configuration: json!({}),
                        secrets: Default::default(),
                        grants: vec![],
                        bindings: vec![],
                        revision: revision(10),
                    }],
                },
                artifacts: vec![
                    artifact(OLD_ARTIFACT, "test.lifecycle", "2.0.0"),
                    artifact(NEW_ARTIFACT, "test.lifecycle", "1.0.0"),
                ],
                artifact_reads: 0,
                change_during_artifact_read: false,
                fail_preparation: false,
                configuration_ready: true,
                prepared_snapshots: vec![],
                saves: vec![],
                history: BTreeMap::from([(
                    NEW_ARTIFACT.into(),
                    PluginVersionConfiguration {
                        configuration: json!({"oldVersion": true}),
                        secrets: BTreeMap::from([("token".into(), "old-version-secret".into())]),
                        bindings: vec![],
                    },
                )]),
                aborted: 0,
                activated: 0,
                quiesced: 0,
                diagnostics: None,
            }),
            next_prepared: AtomicU64::new(1),
        })
    }

    fn snapshot(&self) -> PluginInstanceSnapshot {
        self.data.lock().unwrap().snapshot.clone()
    }

    fn save(
        &self,
        mut instance: PluginInstance,
        expected: Revision,
        transition_id: Option<String>,
        replacements: &[PluginInstanceReplacement],
    ) -> AdminStoreResult<PluginInstanceMutation> {
        let mut data = self.data.lock().unwrap();
        if data.snapshot.config_revision != expected {
            return Err(admin_error(AdminStoreErrorKind::Conflict));
        }
        let next = revision(expected.get() + 1);
        for replacement in replacements {
            let current = data
                .snapshot
                .instances
                .iter_mut()
                .find(|item| item.id == replacement.id)
                .unwrap();
            assert!(current.enabled);
            assert_eq!(current.revision.get(), replacement.expected_revision);
            current.enabled = false;
            current.revision = next;
        }
        if instance.enabled {
            data.history.insert(
                instance.artifact_sha256.clone(),
                PluginVersionConfiguration {
                    configuration: instance.configuration.clone(),
                    secrets: instance.secrets.clone(),
                    bindings: instance.bindings.clone(),
                },
            );
        }
        instance.revision = next;
        data.snapshot.config_revision = next;
        data.snapshot
            .instances
            .retain(|current| current.id != instance.id);
        data.snapshot.instances.push(instance.clone());
        data.saves.push(SavedInstance {
            artifact_sha256: instance.artifact_sha256.clone(),
            enabled: instance.enabled,
            transition_id,
        });
        Ok(PluginInstanceMutation {
            config_revision: next,
            instance,
        })
    }
}

struct Lease;

impl ExtensionSetLease for Lease {
    fn is_ready(&self) -> bool {
        true
    }
}

#[async_trait]
impl PluginPreparation for LifecycleFixture {
    async fn configuration_ready(
        &self,
        instance: PluginInstance,
        metadata: &gateway_admin::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError> {
        assert_eq!(instance.artifact_sha256, metadata.sha256);
        Ok(self.data.lock().unwrap().configuration_ready)
    }

    async fn validate(
        &self,
        instance: PluginInstance,
    ) -> Result<PluginStateConfiguration, AdminError> {
        Ok(state(if instance.artifact_sha256 == OLD_ARTIFACT {
            1
        } else {
            2
        }))
    }

    async fn prepare(
        &self,
        source_revision: Revision,
        snapshot: PluginInstanceSnapshot,
    ) -> Result<ExtensionSetReference, AdminError> {
        let mut data = self.data.lock().unwrap();
        if source_revision != data.snapshot.config_revision {
            return Err(AdminError::conflict(
                "configuration changed during preparation",
            ));
        }
        if data.fail_preparation {
            return Err(AdminError::invalid("候选配置准备失败"));
        }
        assert!(
            snapshot
                .instances
                .iter()
                .all(|instance| instance.revision <= snapshot.config_revision),
            "候选集合必须包含本次实例变更的 revision，才能严格拒绝目标插件启动失败"
        );
        data.prepared_snapshots.push(snapshot);
        let id = self.next_prepared.fetch_add(1, Ordering::Relaxed);
        Ok(ExtensionSetReference::new(
            ExtensionSetId::new(format!("state-candidate-{id}")).unwrap(),
            Arc::new(Lease),
        ))
    }

    async fn runtime_diagnostics(
        &self,
        _: &PluginInstanceSnapshot,
        _: Option<u64>,
        _: Option<&ExtensionSetReference>,
    ) -> Option<BTreeMap<String, PluginInstanceRuntime>> {
        self.data.lock().unwrap().diagnostics.clone()
    }

    async fn activate_state(
        &self,
        _: &ExtensionSetReference,
        _: &PluginInstance,
    ) -> Result<(), AdminError> {
        self.data.lock().unwrap().activated += 1;
        Ok(())
    }

    async fn quiesce_instance(&self, _: &str, _: &str, _: Revision) {
        self.data.lock().unwrap().quiesced += 1;
    }

    async fn migrate_state(
        &self,
        _: &ExtensionSetReference,
        _: PluginStateTransition,
    ) -> Result<(), AdminError> {
        match self.behavior {
            MigrationBehavior::Succeed => Ok(()),
            MigrationBehavior::Fail => Err(AdminError::invalid("migration fixture failed")),
            MigrationBehavior::ConcurrentChangeThenFail => {
                let mut data = self.data.lock().unwrap();
                let next = revision(data.snapshot.config_revision.get() + 1);
                data.snapshot.config_revision = next;
                let instance = &mut data.snapshot.instances[0];
                instance.name = "concurrent edit".into();
                instance.revision = next;
                Err(AdminError::invalid("migration fixture failed"))
            }
        }
    }
}

#[async_trait]
impl PluginStateStore for LifecycleFixture {
    async fn load_owner(
        &self,
        _: PluginStateOwnerRequest,
    ) -> PluginStateStoreResult<Option<PluginStateOwner>> {
        state_unavailable()
    }

    async fn get(
        &self,
        _: &PluginStateOwner,
        _: &str,
        _: &str,
    ) -> PluginStateStoreResult<Option<PluginStateRecord>> {
        state_unavailable()
    }

    async fn put(
        &self,
        _: &PluginStateOwner,
        _: PutPluginState,
    ) -> PluginStateStoreResult<PluginStateWrite> {
        state_unavailable()
    }

    async fn delete(
        &self,
        _: &PluginStateOwner,
        _: DeletePluginState,
    ) -> PluginStateStoreResult<bool> {
        state_unavailable()
    }

    async fn transition_required(
        &self,
        _: &str,
        target: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool> {
        Ok(target.namespaces[0].schema_version == 2)
    }

    async fn begin_transition(
        &self,
        instance_id: &str,
        expected_instance_revision: Revision,
        artifact_sha256: &str,
        _: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition> {
        let data = self.data.lock().unwrap();
        let current = &data.snapshot.instances[0];
        if current.enabled
            || current.id != instance_id
            || current.revision != expected_instance_revision
            || artifact_sha256 != NEW_ARTIFACT
        {
            return Err(PluginStateStoreError::new(
                PluginStateStoreErrorKind::Conflict,
            ));
        }
        Ok(PluginStateTransition {
            id: "00000000-0000-7000-8000-000000000002".into(),
            instance_id: instance_id.into(),
            artifact_sha256: artifact_sha256.into(),
            namespaces: vec![PluginStateMigrationNamespace {
                namespace: "cache".into(),
                from_schema_version: 1,
                to_schema_version: 2,
            }],
        })
    }

    async fn migration_batch(
        &self,
        _: &str,
        _: &str,
        _: u32,
    ) -> PluginStateStoreResult<PluginStateMigrationBatch> {
        state_unavailable()
    }

    async fn apply_migration_batch(
        &self,
        _: ApplyPluginStateMigration,
    ) -> PluginStateStoreResult<()> {
        state_unavailable()
    }

    async fn abort_transition(&self, _: &str) -> PluginStateStoreResult<()> {
        self.data.lock().unwrap().aborted += 1;
        Ok(())
    }
}

#[async_trait]
impl PluginStore for LifecycleFixture {
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
        Ok(self.snapshot())
    }

    async fn load_version_configuration(
        &self,
        _: &str,
        digest: &str,
    ) -> AdminStoreResult<Option<PluginVersionConfiguration>> {
        Ok(self.data.lock().unwrap().history.get(digest).cloned())
    }
    async fn configuration_versions(&self, _: &str) -> AdminStoreResult<Vec<String>> {
        Ok(self.data.lock().unwrap().history.keys().cloned().collect())
    }

    async fn save_instance(
        &self,
        instance: PluginInstance,
        expected_revision: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        self.save(instance, expected_revision, None, &[])
    }

    async fn save_instance_with_state(
        &self,
        instance: PluginInstance,
        expected_revision: Revision,
        state: PluginStateCommit,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        self.save(instance, expected_revision, state.transition_id, &[])
    }

    async fn save_instance_replacing(
        &self,
        instance: PluginInstance,
        expected_revision: Revision,
        state: PluginStateCommit,
        replacements: &[PluginInstanceReplacement],
        _: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        self.save(
            instance,
            expected_revision,
            state.transition_id,
            replacements,
        )
    }

    async fn delete_instance(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn change_update_source(
        &self,
        _: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn save_source_credential(
        &self,
        _: SourceCredential,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn delete_source_credential(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        let mut data = self.data.lock().unwrap();
        data.artifact_reads += 1;
        if data.change_during_artifact_read {
            let next = revision(data.snapshot.config_revision.get() + 1);
            data.snapshot.config_revision = next;
            data.snapshot.instances[0].revision = next;
            data.snapshot.instances[0].name = "concurrent edit".into();
        }
        Ok(data.artifacts.clone())
    }

    async fn load_artifact(&self, _: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn install_artifact(
        &self,
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn accept_artifact(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }

    async fn delete_artifact(&self, _: &str, _: &MutationContext) -> AdminStoreResult<Revision> {
        Err(admin_error(AdminStoreErrorKind::Unavailable))
    }
}

#[derive(Default)]
struct Published {
    revisions: Mutex<Vec<u64>>,
}

impl SnapshotControl for Published {
    fn publish_committed(&self, revision: ConfigRevision) -> BoxFuture<'_, ()> {
        self.revisions.lock().unwrap().push(revision.get());
        Box::pin(async {})
    }
}

fn service(fixture: Arc<LifecycleFixture>, published: Arc<Published>) -> PluginsService {
    PluginsService::new(
        fixture.clone(),
        Arc::new(TestPluginPorts),
        PluginDistributionPorts::new(Arc::new(TestPluginPorts), Arc::new(TestPluginPorts)),
        published,
        fixture.clone(),
        RuntimeSnapshotHandle::default(),
        fixture,
    )
}

fn state(version: u32) -> PluginStateConfiguration {
    PluginStateConfiguration {
        namespaces: vec![PluginStateSchema {
            namespace: "cache".into(),
            schema_version: version,
            schema_sha256: char::from_digit(version, 16)
                .unwrap()
                .to_string()
                .repeat(64),
            schema: json!({"type":"object"}),
            maximum_records: 16,
            maximum_bytes: 1024,
            maximum_value_bytes: 128,
            migrates_from: (version == 2).then_some(1).into_iter().collect(),
        }],
    }
}

fn artifact(digest: &str, plugin_id: &str, version: &str) -> InstalledPluginArtifact {
    InstalledPluginArtifact {
        metadata: PluginArtifactMetadata {
            plugin_id: plugin_id.into(),
            version: version.into(),
            name: plugin_id.strip_prefix("test.").unwrap_or(plugin_id).into(),
            display_name: "Lifecycle fixture".into(),
            publisher: "test".into(),
            author: None,
            description: String::new(),
            license: "MIT".into(),
            sha256: digest.into(),
            platforms: vec!["linux/x86_64".into()],
            icon: None,
            contributes: BTreeMap::from([(
                "middleware".into(),
                PluginContribution {
                    id: format!("{plugin_id}.middleware"),
                    version: 1,
                    stages: vec!["request".into()],
                    input_formats: Vec::new(),
                    output_formats: Vec::new(),
                },
            )]),
            requested_permissions: vec![],
            configuration_schema: json!({"type":"object"}),
            secret_fields: vec![],
            state_namespaces: vec![],
        },
        source: PluginSource::Upload,
        installed_at: chrono::Utc::now(),
        accepted_at: Some(chrono::Utc::now()),
    }
}

fn rollback_input() -> RollbackPluginInstance {
    RollbackPluginInstance {
        artifact_sha256: NEW_ARTIFACT.into(),
        expected_revision: 10,
    }
}

fn input() -> ConfigurePluginInstance {
    ConfigurePluginInstance {
        replace_instances: Vec::new(),
        creation_id: None,
        expected_revision: None,
        name: "upgraded".into(),
        artifact_sha256: NEW_ARTIFACT.into(),
        enabled: true,
        configuration: json!({}),
        secrets: None,
        bindings: vec![],
    }
}

#[tokio::test]
async fn instance_list_loads_metadata_once_for_shared_artifacts_without_loading_archives() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let mut second = fixture.snapshot().instances[0].clone();
    second.id = "00000000-0000-7000-8000-000000000002".into();
    second.enabled = false;
    fixture.data.lock().unwrap().snapshot.instances.push(second);
    let views = service(fixture.clone(), Arc::new(Published::default()))
        .instances()
        .await
        .unwrap();
    assert_eq!(views.len(), 2);
    assert!(views.iter().all(|view| !view.configuration_required));
    assert_eq!(fixture.data.lock().unwrap().artifact_reads, 1);
}

#[tokio::test]
async fn enable_replacement_prepares_and_publishes_one_atomic_switch() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let published = Arc::new(Published::default());
    let service = service(fixture.clone(), published.clone());
    let previous = fixture.snapshot().instances[0].clone();
    let mut request = input();
    request.replace_instances = vec![PluginInstanceReplacement {
        id: previous.id.clone(),
        expected_revision: 10,
    }];
    let result = service
        .configure_instance(None, request, &context())
        .await
        .unwrap();
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.saves.len(), 1);
    assert_eq!(*published.revisions.lock().unwrap(), vec![11]);
    for snapshot in data.prepared_snapshots.iter().chain([&data.snapshot]) {
        let old = snapshot
            .instances
            .iter()
            .find(|item| item.id == previous.id)
            .unwrap();
        let current = snapshot
            .instances
            .iter()
            .find(|item| item.id == result.instance.id)
            .unwrap();
        assert!(!old.enabled);
        assert!(current.enabled);
        assert_eq!(old.revision, current.revision);
        assert_eq!(old.configuration, previous.configuration);
        assert_eq!(old.artifact_sha256, previous.artifact_sha256);
    }
}

#[tokio::test]
async fn enable_replacement_rejects_stale_or_invalid_confirmation_without_writing() {
    for scenario in [
        "stale",
        "missing",
        "disabled",
        "foreign",
        "duplicate",
        "self",
        "save_disabled",
    ] {
        let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
        let published = Arc::new(Published::default());
        let service = service(fixture.clone(), published.clone());
        let previous = fixture.snapshot().instances[0].clone();
        let mut request = input();
        request.replace_instances = vec![PluginInstanceReplacement {
            id: previous.id.clone(),
            expected_revision: 10,
        }];
        let mut target_id = None;
        match scenario {
            "stale" => request.replace_instances[0].expected_revision = 9,
            "missing" => request.replace_instances[0].id = uuid::Uuid::now_v7().to_string(),
            "disabled" => fixture.data.lock().unwrap().snapshot.instances[0].enabled = false,
            "foreign" => {
                fixture.data.lock().unwrap().artifacts[0].metadata.plugin_id =
                    "another.plugin".into()
            }
            "duplicate" => request
                .replace_instances
                .push(request.replace_instances[0].clone()),
            "self" => target_id = Some(previous.id.as_str()),
            "save_disabled" => request.enabled = false,
            _ => unreachable!(),
        }
        assert!(
            service
                .configure_instance(target_id, request, &context())
                .await
                .is_err(),
            "{scenario}"
        );
        assert!(fixture.data.lock().unwrap().saves.is_empty(), "{scenario}");
        assert!(published.revisions.lock().unwrap().is_empty(), "{scenario}");
    }
}

#[tokio::test]
async fn enable_replacement_preparation_failure_preserves_the_enabled_configuration() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    fixture.data.lock().unwrap().fail_preparation = true;
    let service = service(fixture.clone(), Arc::new(Published::default()));
    let previous = fixture.snapshot().instances[0].clone();
    let mut request = input();
    request.replace_instances = vec![PluginInstanceReplacement {
        id: previous.id.clone(),
        expected_revision: 10,
    }];
    assert!(
        service
            .configure_instance(None, request, &context())
            .await
            .is_err()
    );
    assert!(fixture.data.lock().unwrap().saves.is_empty());
    assert!(fixture.snapshot().instances[0].enabled);
}

#[tokio::test]
async fn enable_replacement_state_migration_only_switches_old_configuration_after_success() {
    for behavior in [MigrationBehavior::Succeed, MigrationBehavior::Fail] {
        let fixture = LifecycleFixture::new(behavior);
        let target_id = fixture.snapshot().instances[0].id.clone();
        let mut old = fixture.snapshot().instances[0].clone();
        old.id = uuid::Uuid::now_v7().to_string();
        {
            let mut data = fixture.data.lock().unwrap();
            data.snapshot.instances[0].enabled = false;
            data.snapshot.instances.push(old.clone());
        }
        let service = service(fixture.clone(), Arc::new(Published::default()));
        let mut request = input();
        request.replace_instances = vec![PluginInstanceReplacement {
            id: old.id.clone(),
            expected_revision: 10,
        }];
        let result = service
            .configure_instance(Some(&target_id), request, &context())
            .await;
        let succeeded = matches!(behavior, MigrationBehavior::Succeed);
        assert_eq!(result.is_ok(), succeeded);
        let snapshot = fixture.snapshot();
        assert_eq!(
            snapshot
                .instances
                .iter()
                .find(|item| item.id == old.id)
                .unwrap()
                .enabled,
            !succeeded
        );
        assert_eq!(
            snapshot
                .instances
                .iter()
                .find(|item| item.id == target_id)
                .unwrap()
                .enabled,
            succeeded
        );
    }
}

#[tokio::test]
async fn configuration_retry_reuses_creation_id_and_rejects_changed_draft() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let service = service(fixture.clone(), Arc::new(Published::default()));
    let creation_id = "00000000-0000-7000-8000-000000000002";
    let request = || {
        let mut value = input();
        value.creation_id = Some(creation_id.into());
        // 此用例验证创建重试，使用不需要私有状态迁移的固定版本。
        value.artifact_sha256 = OLD_ARTIFACT.into();
        value.enabled = false;
        value
    };
    let created = service
        .configure_instance(None, request(), &context())
        .await
        .unwrap();
    let retried = service
        .configure_instance(None, request(), &context())
        .await
        .unwrap();
    assert_eq!(created.instance.id, creation_id);
    assert_eq!(retried.instance.id, creation_id);
    assert_eq!(fixture.snapshot().instances.len(), 2);
    let before = fixture.snapshot().config_revision;
    let mut changed = request();
    changed.name = "changed draft".into();
    let error = service
        .configure_instance(None, changed, &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert_eq!(fixture.snapshot().config_revision, before);
}

#[tokio::test]
async fn configuration_update_rejects_stale_revision_without_writing() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let mut request = input();
    request.expected_revision = Some(9);
    let error = service(fixture.clone(), Arc::new(Published::default()))
        .configure_instance(
            Some("00000000-0000-7000-8000-000000000001"),
            request,
            &context(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert!(fixture.data.lock().unwrap().saves.is_empty());
}

fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "state-lifecycle-test".into(),
    }
}

fn revision(value: u64) -> Revision {
    Revision::new(value).unwrap()
}

fn admin_error(kind: AdminStoreErrorKind) -> AdminStoreError {
    AdminStoreError::new(kind, "plugin", "state lifecycle fixture")
}

fn state_unavailable<T>() -> PluginStateStoreResult<T> {
    Err(PluginStateStoreError::new(
        PluginStateStoreErrorKind::Unavailable,
    ))
}

#[tokio::test]
async fn instance_list_uses_runtime_diagnostics_and_keeps_running_compatibility_derived() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let expected = PluginInstanceRuntime {
        status: PluginInstanceRuntimeStatus::PreparationFailed,
        actual_revision: Some(9),
        actual_artifact_sha256: Some(OLD_ARTIFACT.into()),
        failure: Some(PluginInstanceRuntimeFailure {
            code: "unavailable".into(),
            message: "插件候选准备失败".into(),
        }),
        draining_revisions: vec![8],
        retained_continuations: 1,
    };
    fixture.data.lock().unwrap().diagnostics = Some(BTreeMap::from([(
        "00000000-0000-7000-8000-000000000001".into(),
        expected.clone(),
    )]));

    let views = service(fixture, Arc::new(Published::default()))
        .instances()
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    assert!(!views[0].running);
    assert_eq!(views[0].runtime, expected);
}

#[tokio::test]
async fn instance_list_falls_back_without_inventing_runtime_failures() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let views = service(fixture, Arc::new(Published::default()))
        .instances()
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].runtime.status,
        PluginInstanceRuntimeStatus::AwaitingPublication
    );
    assert!(!views[0].running);
    assert!(views[0].runtime.failure.is_none());
}

#[tokio::test]
async fn incompatible_state_upgrade_quiesces_migrates_and_promotes_the_target() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let published = Arc::new(Published::default());
    let result = service(fixture.clone(), published.clone())
        .configure_instance(
            Some("00000000-0000-7000-8000-000000000001"),
            input(),
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(result.config_revision, revision(12));
    assert_eq!(result.instance.artifact_sha256, NEW_ARTIFACT);
    assert!(result.instance.enabled);
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.quiesced, 1);
    assert_eq!(data.activated, 1);
    assert_eq!(data.aborted, 0);
    assert_eq!(data.saves.len(), 2);
    assert_eq!(data.saves[0].artifact_sha256, OLD_ARTIFACT);
    assert!(!data.saves[0].enabled);
    assert!(data.saves[0].transition_id.is_none());
    assert_eq!(data.saves[1].artifact_sha256, NEW_ARTIFACT);
    assert!(data.saves[1].enabled);
    assert!(data.saves[1].transition_id.is_some());
    assert_eq!(published.revisions.lock().unwrap().as_slice(), [11, 12]);
}

#[tokio::test]
async fn failed_state_migration_restores_the_previous_enabled_version_with_cas() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Fail);
    let published = Arc::new(Published::default());
    let error = service(fixture.clone(), published.clone())
        .configure_instance(
            Some("00000000-0000-7000-8000-000000000001"),
            input(),
            &context(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.snapshot.config_revision, revision(12));
    assert_eq!(data.snapshot.instances[0].artifact_sha256, OLD_ARTIFACT);
    assert!(data.snapshot.instances[0].enabled);
    assert_eq!(data.quiesced, 1);
    assert_eq!(data.activated, 1);
    assert_eq!(data.aborted, 1);
    assert_eq!(data.saves.len(), 2);
    assert!(!data.saves[0].enabled);
    assert!(data.saves[1].enabled);
    assert_eq!(published.revisions.lock().unwrap().as_slice(), [11, 12]);
}

#[tokio::test]
async fn failed_state_migration_never_overwrites_a_concurrent_configuration_change() {
    let fixture = LifecycleFixture::new(MigrationBehavior::ConcurrentChangeThenFail);
    let published = Arc::new(Published::default());
    let error = service(fixture.clone(), published.clone())
        .configure_instance(
            Some("00000000-0000-7000-8000-000000000001"),
            input(),
            &context(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.snapshot.config_revision, revision(12));
    assert_eq!(data.snapshot.instances[0].name, "concurrent edit");
    assert_eq!(data.snapshot.instances[0].artifact_sha256, OLD_ARTIFACT);
    assert!(!data.snapshot.instances[0].enabled);
    assert_eq!(data.aborted, 1);
    assert_eq!(data.activated, 0);
    assert_eq!(data.saves.len(), 1, "恢复不能覆盖迁移期间的新配置");
    assert_eq!(published.revisions.lock().unwrap().as_slice(), [11]);
}

#[tokio::test]
async fn rollback_restores_target_configuration_secrets_and_bindings_and_derives_permissions() {
    use secrecy::ExposeSecret as _;

    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    {
        let mut data = fixture.data.lock().unwrap();
        data.artifacts[1].metadata.requested_permissions = vec!["network".into()];
        let instance = &mut data.snapshot.instances[0];
        instance.configuration = json!({"label":"retained"});
        instance
            .secrets
            .insert("token".into(), "fixture-secret".into());
        instance.bindings.push(PluginCapabilityBinding {
            contribution: "test.lifecycle.middleware".into(),
            stage: "request".into(),
            order: 7,
            failure_policy: PluginFailurePolicy::Reject,
            client_key_ids: Vec::new(),
            account_group_ids: Vec::new(),
            provider_ids: Vec::new(),
            models: Vec::new(),
            identity_bindings: Vec::new(),
        });
    }
    let before = fixture.snapshot().instances.remove(0);
    let result = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_instance(&before.id, rollback_input(), &context())
        .await
        .unwrap();
    assert_eq!(result.instance.artifact_sha256, NEW_ARTIFACT);
    assert_eq!(result.instance.name, before.name);
    assert_eq!(result.instance.enabled, before.enabled);
    assert_eq!(result.instance.configuration, json!({"oldVersion": true}));
    assert_eq!(
        result.instance.grants,
        [PluginPermissionGrant {
            permission: "network".into()
        }]
    );
    assert!(result.instance.bindings.is_empty());
    assert_eq!(
        result.instance.secrets["token"].expose_secret(),
        "old-version-secret"
    );
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.quiesced, 1);
    assert_eq!(data.activated, 1);
    assert!(data.saves.last().unwrap().transition_id.is_some());
}

#[tokio::test]
async fn rollback_rejects_stale_confirmation_without_preparing_or_writing() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let mut input = rollback_input();
    input.expected_revision = 9;
    let error = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_instance(&fixture.snapshot().instances[0].id, input, &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert!(fixture.data.lock().unwrap().saves.is_empty());
    assert_eq!(fixture.next_prepared.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn rollback_rejects_foreign_newer_equal_or_uninstalled_artifacts() {
    for (plugin_id, version, installed, expected) in [
        ("test.other-plugin", "1.0.0", true, AdminErrorKind::Invalid),
        ("test.lifecycle", "2.0.1", true, AdminErrorKind::Invalid),
        ("test.lifecycle", "2.0.0", true, AdminErrorKind::Invalid),
        (
            "test.lifecycle",
            "2.0.0+older-build",
            true,
            AdminErrorKind::Invalid,
        ),
        ("test.lifecycle", "1.0.0", false, AdminErrorKind::NotFound),
    ] {
        let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
        {
            let mut data = fixture.data.lock().unwrap();
            data.artifacts.truncate(1);
            if installed {
                data.artifacts
                    .push(artifact(NEW_ARTIFACT, plugin_id, version));
            }
        }
        let error = service(fixture.clone(), Arc::new(Published::default()))
            .rollback_instance(
                &fixture.snapshot().instances[0].id,
                rollback_input(),
                &context(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), expected, "{plugin_id}/{version}/{installed}");
        assert!(fixture.data.lock().unwrap().saves.is_empty());
        assert_eq!(fixture.next_prepared.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn rollback_keeps_the_original_snapshot_fence_during_artifact_lookup() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    fixture.data.lock().unwrap().change_during_artifact_read = true;
    let error = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_instance(
            &fixture.snapshot().instances[0].id,
            rollback_input(),
            &context(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.snapshot.instances[0].name, "concurrent edit");
    assert_eq!(data.snapshot.instances[0].artifact_sha256, OLD_ARTIFACT);
    assert!(data.saves.is_empty());
}

#[tokio::test]
async fn rollback_migration_failure_restores_the_previous_version_without_replaying() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Fail);
    let error = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_instance(
            &fixture.snapshot().instances[0].id,
            rollback_input(),
            &context(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.snapshot.instances[0].artifact_sha256, OLD_ARTIFACT);
    assert!(data.snapshot.instances[0].enabled);
    assert_eq!(data.aborted, 1);
    assert!(
        data.saves
            .iter()
            .all(|saved| saved.artifact_sha256 == OLD_ARTIFACT)
    );
}

#[tokio::test]
async fn rollback_of_a_disabled_instance_never_enables_it() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    fixture.data.lock().unwrap().snapshot.instances[0].enabled = false;
    let result = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_instance(
            &fixture.snapshot().instances[0].id,
            rollback_input(),
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(result.instance.artifact_sha256, NEW_ARTIFACT);
    assert!(!result.instance.enabled);
    assert!(
        fixture
            .data
            .lock()
            .unwrap()
            .saves
            .iter()
            .all(|saved| !saved.enabled)
    );
}

#[tokio::test]
async fn rollback_plan_is_read_only_and_orders_only_earlier_versions_of_the_same_plugin() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    {
        let mut data = fixture.data.lock().unwrap();
        data.artifacts.extend([
            artifact(&"c".repeat(64), "test.lifecycle", "2.0.0-rc.2"),
            artifact(&"d".repeat(64), "test.lifecycle", "2.0.0-rc.10"),
            artifact(&"e".repeat(64), "test.lifecycle", "2.0.0+another-build"),
            artifact(&"f".repeat(64), "test.other-plugin", "0.1.0"),
        ]);
        let saved = data.history[NEW_ARTIFACT].clone();
        data.history.insert("c".repeat(64), saved.clone());
        data.history.insert("d".repeat(64), saved);
        data.snapshot.instances[0]
            .secrets
            .insert("token".into(), "fixture-secret".into());
    }
    let plan = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_plan(&fixture.snapshot().instances[0].id)
        .await
        .unwrap();
    assert_eq!(plan.instance_revision, 10);
    assert_eq!(plan.current_version, "2.0.0");
    assert_eq!(
        plan.targets
            .iter()
            .map(|target| target.version.as_str())
            .collect::<Vec<_>>(),
        ["2.0.0-rc.10", "2.0.0-rc.2", "1.0.0"],
    );
    let public = serde_json::to_string(&plan).unwrap();
    assert!(!public.contains("fixture-secret"));
    assert!(!public.contains("token"));
    assert!(fixture.data.lock().unwrap().saves.is_empty());
    assert_eq!(fixture.next_prepared.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn rollback_plan_excludes_unaccepted_older_artifacts() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let digest = "c".repeat(64);
    let mut pending = artifact(&digest, "test.lifecycle", "1.5.0");
    pending.accepted_at = None;
    fixture.data.lock().unwrap().artifacts.push(pending);

    let plan = service(fixture.clone(), Arc::new(Published::default()))
        .rollback_plan(&fixture.snapshot().instances[0].id)
        .await
        .unwrap();

    assert!(
        plan.targets
            .iter()
            .all(|target| target.artifact_sha256 != digest)
    );
}

#[tokio::test]
async fn enabling_another_instance_requires_an_explicit_replacement() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let error = service(fixture.clone(), Arc::new(Published::default()))
        .configure_instance(None, input(), &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert!(fixture.data.lock().unwrap().saves.is_empty());
    assert!(fixture.snapshot().instances[0].enabled);
}

#[tokio::test]
async fn version_plan_keeps_explicit_values_adds_defaults_and_remaps_bindings_without_writing() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    {
        let mut data = fixture.data.lock().unwrap();
        data.history.clear();
        data.artifacts[1].metadata.configuration_schema = json!({
            "type":"object", "default":{"fromRoot":42}, "properties": {
                "label":{"type":"string","default":"default"},
                "nested":{"type":"object","properties":{"added":{"default":true},"kept":{"default":100}}},
                "token":{"type":"string","default":"never-copy-secret"}
            }
        });
        data.artifacts[1].metadata.secret_fields = vec!["token".into()];
        data.artifacts[1]
            .metadata
            .contributes
            .get_mut("middleware")
            .unwrap()
            .id = "renamed.middleware".into();
        data.snapshot.instances[0].configuration =
            json!({"label":"mine","nested":{"kept":0},"obsolete":"keep-for-validation"});
        data.snapshot.instances[0]
            .secrets
            .insert("token".into(), "private-value".into());
        data.snapshot.instances[0].bindings = vec![PluginCapabilityBinding {
            contribution: "test.lifecycle.middleware".into(),
            stage: "request".into(),
            order: 3,
            failure_policy: PluginFailurePolicy::Reject,
            client_key_ids: vec![],
            account_group_ids: vec![],
            provider_ids: vec![],
            models: vec!["test-model".into()],
            identity_bindings: vec![],
        }];
    }
    let before = fixture.snapshot();
    let plan = service(fixture.clone(), Arc::new(Published::default()))
        .version_plan(&before.instances[0].id, NEW_ARTIFACT)
        .await
        .unwrap();
    assert!(!plan.restored);
    assert_eq!(
        plan.configuration,
        json!({"label":"mine","nested":{"kept":0,"added":true},"obsolete":"keep-for-validation","fromRoot":42})
    );
    assert_eq!(plan.bindings[0].contribution, "renamed.middleware");
    assert_eq!(plan.bindings[0].models, ["test-model"]);
    assert_eq!(plan.secret_fields, ["token"]);
    let public = serde_json::to_string(&plan).unwrap();
    assert!(!public.contains("private-value"));
    assert!(!public.contains("never-copy-secret"));
    assert_eq!(fixture.snapshot().config_revision, before.config_revision);
    assert!(fixture.data.lock().unwrap().saves.is_empty());
}

#[tokio::test]
async fn version_switch_preparation_failure_preserves_current_and_saved_settings() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    fixture.data.lock().unwrap().fail_preparation = true;
    let before = fixture.snapshot();
    let result = service(fixture.clone(), Arc::new(Published::default()))
        .switch_instance_version(&before.instances[0].id, rollback_input(), &context())
        .await;
    assert!(result.is_err());
    assert_eq!(fixture.snapshot().config_revision, before.config_revision);
    assert_eq!(
        fixture.snapshot().instances[0].artifact_sha256,
        OLD_ARTIFACT
    );
    let data = fixture.data.lock().unwrap();
    assert!(data.saves.is_empty());
    assert_eq!(
        data.history[NEW_ARTIFACT].configuration,
        json!({"oldVersion":true})
    );
}

#[tokio::test]
async fn version_edit_preserves_the_target_versions_secrets_when_omitted() {
    use secrecy::ExposeSecret as _;
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let before = fixture.snapshot();
    let mut request = input();
    request.secrets = None;
    let result = service(fixture, Arc::new(Published::default()))
        .configure_instance(Some(&before.instances[0].id), request, &context())
        .await
        .unwrap();
    assert_eq!(
        result.instance.secrets["token"].expose_secret(),
        "old-version-secret"
    );
}

#[tokio::test]
async fn rollback_requires_a_saved_configuration_instead_of_reusing_incompatible_current_settings()
{
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    fixture.data.lock().unwrap().history.clear();
    let service = service(fixture.clone(), Arc::new(Published::default()));
    let id = fixture.snapshot().instances[0].id.clone();
    assert!(service.rollback_plan(&id).await.unwrap().targets.is_empty());
    let error = service
        .rollback_instance(&id, rollback_input(), &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
    assert!(fixture.data.lock().unwrap().saves.is_empty());
}

#[tokio::test]
async fn switching_a_disabled_plugin_also_requires_complete_target_settings() {
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    {
        let mut data = fixture.data.lock().unwrap();
        data.configuration_ready = false;
        data.snapshot.instances[0].enabled = false;
    }
    let before = fixture.snapshot();
    let error = service(fixture.clone(), Arc::new(Published::default()))
        .switch_instance_version(&before.instances[0].id, rollback_input(), &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Invalid);
    assert_eq!(fixture.snapshot().config_revision, before.config_revision);
    assert!(fixture.data.lock().unwrap().saves.is_empty());
}

#[tokio::test]
async fn switching_versions_uses_saved_settings_and_rejects_stale_retry() {
    use secrecy::ExposeSecret as _;
    let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
    let service = service(fixture.clone(), Arc::new(Published::default()));
    let id = fixture.snapshot().instances[0].id.clone();
    let result = service
        .switch_instance_version(&id, rollback_input(), &context())
        .await
        .unwrap();
    assert_eq!(result.instance.artifact_sha256, NEW_ARTIFACT);
    assert_eq!(result.instance.configuration, json!({"oldVersion":true}));
    assert_eq!(
        result.instance.secrets["token"].expose_secret(),
        "old-version-secret"
    );
    assert!(result.instance.enabled);
    let revision = fixture.snapshot().config_revision;
    let error = service
        .switch_instance_version(&id, rollback_input(), &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::Conflict);
    assert_eq!(fixture.snapshot().config_revision, revision);
}
