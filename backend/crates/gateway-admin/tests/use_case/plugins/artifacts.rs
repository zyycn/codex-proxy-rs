//! 插件制品安装、接受、默认实例与发布行为的用例测试

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    PluginDistributionPorts, PluginsService,
    model::{
        AdminError, MutationActor, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMetadata,
            PluginArtifactMutation, PluginContribution, PluginSource,
            distribution::{PluginSourceBinding, SourceCredential, SourceCredentialInfo},
            instances::{PluginInstance, PluginInstanceMutation, PluginInstanceSnapshot},
            state::PluginStateConfiguration,
        },
    },
    ports::{
        plugins::{PluginPreparation, PluginStore},
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

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct FixtureData {
    artifact: InstalledPluginArtifact,
    snapshot: PluginInstanceSnapshot,
    saves: usize,
    publications: Vec<u64>,
}

struct Fixture {
    data: Mutex<FixtureData>,
}

impl Fixture {
    fn new(metadata: PluginArtifactMetadata, existing: Option<PluginInstance>) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(FixtureData {
                artifact: InstalledPluginArtifact {
                    metadata,
                    source: PluginSource::Upload,
                    installed_at: chrono::Utc::now(),
                    accepted_at: None,
                },
                snapshot: PluginInstanceSnapshot {
                    config_revision: revision(1),
                    instances: existing.into_iter().collect(),
                },
                saves: 0,
                publications: Vec::new(),
            }),
        })
    }

    fn service(self: &Arc<Self>) -> PluginsService {
        let unused = Arc::new(TestPluginPorts);
        PluginsService::new(
            self.clone(),
            self.clone(),
            PluginDistributionPorts::new(unused.clone(), unused.clone()),
            self.clone(),
            self.clone(),
            RuntimeSnapshotHandle::default(),
            unused,
        )
    }
}

impl SnapshotControl for Fixture {
    fn publish_committed(&self, revision: ConfigRevision) -> BoxFuture<'_, ()> {
        self.data.lock().unwrap().publications.push(revision.get());
        Box::pin(async {})
    }
}

#[async_trait]
impl gateway_admin::ports::plugins::PluginPackageInspector for Fixture {
    async fn inspect(
        &self,
        archive: Arc<[u8]>,
        _: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError> {
        Ok(InspectedPluginArtifact {
            metadata: self.data.lock().unwrap().artifact.metadata.clone(),
            archive,
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
impl gateway_admin::ports::plugins::PluginRuntimeDiagnostics for Fixture {
    async fn runtime_diagnostics(
        &self,
        _: &gateway_admin::model::plugins::instances::PluginInstanceSnapshot,
        _: Option<u64>,
        _: Option<&gateway_core::runtime::extensions::ExtensionSetReference>,
    ) -> Option<
        std::collections::BTreeMap<
            String,
            gateway_admin::model::plugins::instances::PluginInstanceRuntime,
        >,
    > {
        None
    }
}

#[async_trait]
impl gateway_admin::ports::plugins::PluginStateLifecycle for Fixture {
    async fn activate_state(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::instances::PluginInstance,
    ) -> Result<(), AdminError> {
        Ok(())
    }
    async fn quiesce_instance(&self, _: &str, _: &str, _: gateway_admin::model::Revision) {
        panic!("unexpected instance drain")
    }
    async fn migrate_state(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: gateway_admin::model::plugins::state::PluginStateTransition,
    ) -> Result<(), AdminError> {
        panic!("unexpected state migration")
    }
}

#[async_trait]
impl PluginPreparation for Fixture {
    async fn configuration_ready(
        &self,
        instance: PluginInstance,
        _: &gateway_admin::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError> {
        Ok(instance
            .configuration
            .get("requiredName")
            .is_some_and(serde_json::Value::is_string))
    }

    async fn validate(&self, _: PluginInstance) -> Result<PluginStateConfiguration, AdminError> {
        Ok(PluginStateConfiguration {
            namespaces: Vec::new(),
        })
    }

    async fn prepare(
        &self,
        _: PluginInstanceSnapshot,
    ) -> Result<ExtensionSetReference, AdminError> {
        Ok(ExtensionSetReference::new(
            ExtensionSetId::new("install-fixture".into()).unwrap(),
            Arc::new(Lease),
        ))
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
        Ok(self.data.lock().unwrap().snapshot.clone())
    }

    async fn save_instance(
        &self,
        mut instance: PluginInstance,
        expected_revision: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        let mut data = self.data.lock().unwrap();
        if data.snapshot.config_revision != expected_revision {
            return Err(store_error(AdminStoreErrorKind::Conflict));
        }
        let next = revision(expected_revision.get() + 1);
        instance.revision = next;
        data.snapshot.config_revision = next;
        data.snapshot
            .instances
            .retain(|current| current.id != instance.id);
        data.snapshot.instances.push(instance.clone());
        data.saves += 1;
        Ok(PluginInstanceMutation {
            config_revision: next,
            instance,
        })
    }

    async fn delete_instance(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }

    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        Ok(Vec::new())
    }

    async fn change_update_source(
        &self,
        _: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }

    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        Ok(Vec::new())
    }

    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }

    async fn save_source_credential(
        &self,
        _: SourceCredential,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }

    async fn delete_source_credential(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }

    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        Ok(vec![self.data.lock().unwrap().artifact.clone()])
    }

    async fn load_artifact(&self, _: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        Ok(InspectedPluginArtifact {
            metadata: self.data.lock().unwrap().artifact.metadata.clone(),
            archive: Arc::from([1_u8]),
        })
    }

    async fn install_artifact(
        &self,
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }

    async fn accept_artifact(
        &self,
        digest: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        let mut data = self.data.lock().unwrap();
        if data.artifact.metadata.sha256 != digest {
            return Err(store_error(AdminStoreErrorKind::NotFound));
        }
        if data.artifact.accepted_at.is_none() {
            data.artifact.accepted_at = Some(chrono::Utc::now());
            data.snapshot.config_revision = revision(data.snapshot.config_revision.get() + 1);
        }
        Ok(PluginArtifactMutation {
            config_revision: data.snapshot.config_revision,
            artifact: data.artifact.clone(),
        })
    }

    async fn delete_artifact(&self, _: &str, _: &MutationContext) -> AdminStoreResult<Revision> {
        Err(store_error(AdminStoreErrorKind::Unavailable))
    }
}

#[tokio::test]
async fn acceptance_creates_one_stable_default_instance_with_defaults_and_bindings() {
    let fixture = Fixture::new(metadata(true), None);
    let service = fixture.service();

    let first = service.accept_artifact(DIGEST, &context()).await.unwrap();
    let repeated = service.accept_artifact(DIGEST, &context()).await.unwrap();
    assert_eq!(first.default_instance_id, repeated.default_instance_id);
    assert!(!first.configuration_required);
    assert!(!repeated.configuration_required);

    let data = fixture.data.lock().unwrap();
    assert_eq!(data.saves, 1, "同一摘要重试不能复制默认实例");
    assert_eq!(data.snapshot.instances.len(), 1);
    let instance = &data.snapshot.instances[0];
    assert_eq!(
        Some(instance.id.as_str()),
        first.default_instance_id.as_deref()
    );
    assert!(instance.enabled);
    assert_eq!(instance.configuration["mode"], "safe");
    assert_eq!(instance.configuration["nested"]["limit"], 3);
    assert_eq!(instance.configuration["requiredName"], "ready");
    assert!(instance.configuration.get("token").is_none());
    assert_eq!(
        instance
            .bindings
            .iter()
            .map(|binding| (
                binding.contribution.as_str(),
                binding.stage.as_str(),
                &binding.failure_policy,
            ))
            .collect::<Vec<_>>(),
        [
            (
                "test.example.middleware",
                "observation",
                &gateway_admin::model::plugins::instances::PluginFailurePolicy::Observe,
            ),
            (
                "test.example.middleware",
                "request",
                &gateway_admin::model::plugins::instances::PluginFailurePolicy::Reject,
            ),
        ]
    );
    assert!(instance.bindings.iter().all(|binding| !matches!(
        binding.contribution.as_str(),
        "test.example.auth" | "test.example.cli" | "test.example.management"
    )));
}

#[tokio::test]
async fn missing_required_configuration_creates_a_disabled_pending_instance() {
    let fixture = Fixture::new(metadata(false), None);
    let result = fixture
        .service()
        .accept_artifact(DIGEST, &context())
        .await
        .unwrap();
    assert!(result.configuration_required);
    assert!(result.default_instance_id.is_some());
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.snapshot.instances.len(), 1);
    assert!(!data.snapshot.instances[0].enabled);
}

#[tokio::test]
async fn existing_plugin_instance_prevents_an_extra_default_instance() {
    let metadata = metadata(true);
    let existing = PluginInstance {
        id: uuid::Uuid::now_v7().to_string(),
        name: "Existing".into(),
        artifact_sha256: metadata.sha256.clone(),
        enabled: false,
        trusted_process: false,
        configuration: json!({}),
        secrets: BTreeMap::new(),

        bindings: Vec::new(),
        revision: revision(1),
    };
    let fixture = Fixture::new(metadata, Some(existing));
    let result = fixture
        .service()
        .accept_artifact(DIGEST, &context())
        .await
        .unwrap();
    assert_eq!(result.default_instance_id, None);
    let data = fixture.data.lock().unwrap();
    assert_eq!(data.saves, 0);
    assert_eq!(data.snapshot.instances.len(), 1);
}

fn metadata(with_required_default: bool) -> PluginArtifactMetadata {
    let required_name = if with_required_default {
        json!({"type":"string","default":"ready"})
    } else {
        json!({"type":"string"})
    };
    PluginArtifactMetadata {
        plugin_id: "test.example".into(),
        version: "1.0.0".into(),
        name: "example".into(),
        display_name: "Example".into(),
        publisher: "test".into(),
        author: None,
        description: "fixture".into(),
        license: "MIT".into(),
        sha256: DIGEST.into(),
        platforms: vec!["linux-x86_64".into()],
        icon: None,
        contributes: BTreeMap::from([
            (
                "command_line".into(),
                contribution("test.example.cli", &["command"]),
            ),
            (
                "frontend_authentication".into(),
                contribution("test.example.auth", &["authentication"]),
            ),
            (
                "management".into(),
                contribution("test.example.management", &["management"]),
            ),
            (
                "middleware".into(),
                contribution("test.example.middleware", &["observation", "request"]),
            ),
        ]),

        configuration_schema: json!({
            "type":"object",
            "properties": {
                "mode": {"type":"string","default":"safe"},
                "nested": {
                    "type":"object",
                    "default": {},
                    "properties": {"limit":{"type":"integer","default":3}}
                },
                "requiredName": required_name,
                "token": {"type":"string","default":"never-copy-secret"}
            },
            "required":["requiredName"]
        }),
        secret_fields: vec!["token".into()],
        state_namespaces: Vec::new(),
    }
}

fn contribution(id: &str, stages: &[&str]) -> PluginContribution {
    PluginContribution {
        id: id.into(),
        version: 1,
        stages: stages.iter().map(|stage| (*stage).into()).collect(),
        input_formats: Vec::new(),
        output_formats: Vec::new(),
    }
}

fn revision(value: u64) -> Revision {
    Revision::new(value).unwrap()
}

fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "install-fixture".into(),
    }
}

fn store_error(kind: AdminStoreErrorKind) -> AdminStoreError {
    AdminStoreError::new(kind, "plugin", "install fixture")
}
