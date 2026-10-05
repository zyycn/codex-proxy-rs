//! 插件参与系统升级与回滚的兼容性预检测试

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMetadata,
            PluginArtifactMutation, PluginCompatibilityRequirements, PluginSource,
            distribution::{PluginSourceBinding, SourceCredential, SourceCredentialInfo},
            instances::{PluginInstance, PluginInstanceMutation, PluginInstanceSnapshot},
        },
        system::{
            SystemOperationAccepted, SystemOperationState, SystemOperationStatus,
            SystemUpdateDetail, SystemUpdateStatus, SystemVersion,
        },
    },
    ports::{
        plugins::{PluginPackageInspector, PluginStore},
        store::AdminStoreResult,
        system::{
            SystemOperationError, SystemOperations, SystemUpdateCandidate, SystemUpdateEventStream,
            SystemUpdatePreflight,
        },
    },
};

struct Fixture {
    revisions: std::sync::Mutex<Vec<Revision>>,
    requirements: PluginCompatibilityRequirements,
    instances: Vec<PluginInstance>,
    disabled: std::sync::Mutex<Vec<String>>,
}

impl Fixture {
    fn new(requirements: PluginCompatibilityRequirements, revisions: &[u64]) -> Arc<Self> {
        Arc::new(Self {
            revisions: std::sync::Mutex::new(
                revisions
                    .iter()
                    .map(|revision| Revision::new(*revision).expect("revision"))
                    .collect(),
            ),
            requirements,
            instances: vec![instance(true)],
            disabled: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn next_revision(&self) -> Revision {
        let mut revisions = self.revisions.lock().expect("revisions");
        if revisions.len() > 1 {
            revisions.remove(0)
        } else {
            *revisions.first().expect("fixture revision")
        }
    }
}

#[async_trait]
impl PluginPackageInspector for Fixture {
    async fn inspect(
        &self,
        _: Arc<[u8]>,
        _: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError> {
        unreachable!()
    }

    async fn compatibility(
        &self,
        _: Arc<[u8]>,
        expected_sha256: String,
    ) -> Result<PluginCompatibilityRequirements, AdminError> {
        assert_eq!(expected_sha256, "a".repeat(64));
        Ok(self.requirements.clone())
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
        Ok(PluginInstanceSnapshot {
            config_revision: self.next_revision(),
            instances: self
                .instances
                .iter()
                .cloned()
                .map(|mut instance| {
                    if self.disabled.lock().unwrap().contains(&instance.id) {
                        instance.enabled = false;
                    }
                    instance
                })
                .collect(),
        })
    }

    async fn load_artifact(&self, digest: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        assert_eq!(digest, "a".repeat(64));
        Ok(InspectedPluginArtifact {
            metadata: PluginArtifactMetadata {
                plugin_id: "test.fixture".into(),
                version: "1.0.0".into(),
                name: "fixture".into(),
                display_name: "Fixture".into(),
                publisher: "test".into(),
                author: Some("project".into()),
                description: "fixture".into(),
                license: "MIT".into(),
                sha256: digest.into(),
                platforms: Vec::new(),
                icon: None,
                contributes: BTreeMap::new(),

                configuration_schema: serde_json::json!({}),
                secret_fields: Vec::new(),
                state_namespaces: Vec::new(),
            },
            archive: Arc::from([1_u8]),
        })
    }

    async fn disable_instances(
        &self,
        ids: &[String],
        expected: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        let mut revisions = self.revisions.lock().unwrap();
        if revisions[0] != expected {
            return Err(gateway_admin::ports::store::AdminStoreError::new(
                gateway_admin::ports::store::AdminStoreErrorKind::StaleRevision,
                "plugin",
                "stale",
            ));
        }
        revisions[0] = Revision::new(expected.get() + 1).unwrap();
        self.disabled.lock().unwrap().extend_from_slice(ids);
        Ok(revisions[0])
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

    async fn install_artifact(
        &self,
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        unreachable!()
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

struct PreflightingSystem {
    candidate: SystemUpdateCandidate,
}

#[async_trait]
impl SystemOperations for PreflightingSystem {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError> {
        unreachable!()
    }

    async fn update_detail(
        &self,
        _: bool,
        _: Option<gateway_admin::model::system::SystemUpdateChannel>,
    ) -> Result<SystemUpdateDetail, SystemOperationError> {
        unreachable!()
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        Box::pin(futures::stream::empty())
    }

    async fn perform_update(
        &self,
        _: Option<String>,
        _: Option<gateway_admin::model::system::SystemUpdateChannel>,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        let revision = preflight.validate(self.candidate.clone()).await?;
        preflight.confirm_revision(revision).await?;
        Ok(SystemOperationAccepted::Update {
            operation_id: "fixture".into(),
            deployment_mode: "fixture".into(),
            message: "accepted".into(),
            target_version: self.candidate.target_version.clone(),
        })
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        Ok(SystemUpdateStatus {
            previous_version: None,
            current_version: None,
            need_restart: false,
            operation: SystemOperationState {
                operation_id: None,
                kind: None,
                status: SystemOperationStatus::Idle,
                target_version: None,
                message: None,
                error: None,
                started_at: None,
                finished_at: None,
            },
        })
    }

    async fn rollback(
        &self,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        let revision = preflight.validate_rollback(self.candidate.clone()).await?;
        preflight.confirm_revision(revision).await?;
        Ok(SystemOperationAccepted::Rollback {
            operation_id: "fixture".into(),
            message: "accepted".into(),
            need_restart: true,
        })
    }

    async fn restart_candidate(
        &self,
    ) -> Result<Option<SystemUpdateCandidate>, SystemOperationError> {
        Ok(Some(self.candidate.clone()))
    }
    async fn restart(
        &self,
        preflight: Arc<dyn gateway_admin::ports::system::SystemRestartPreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        preflight.prepare(Some(self.candidate.clone())).await?;
        Ok(SystemOperationAccepted::Restart {
            operation_id: "fixture".into(),
            message: "accepted".into(),
        })
    }
}

#[tokio::test]
async fn system_update_accepts_compatible_enabled_plugins_at_the_same_revision() {
    let fixture = Fixture::new(requirements("^1.0", "executor"), &[7]);
    let services = super::AdminHarness::new()
        .plugins(fixture.clone(), fixture)
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("executor"),
        }))
        .build()
        .await;

    services
        .system()
        .perform_update(Some("1.2.0".into()), None)
        .await
        .expect("compatible update");
}

#[tokio::test]
async fn rollback_preserves_plugins_when_target_capability_is_missing() {
    let incompatible = Fixture::new(requirements("^1.0", "executor"), &[7]);
    let services = super::AdminHarness::new()
        .plugins(incompatible.clone(), incompatible.clone())
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("models"),
        }))
        .build()
        .await;
    services
        .system()
        .rollback()
        .await
        .expect("兼容性风险不阻止回滚");
    assert!(incompatible.disabled.lock().unwrap().is_empty());
}

#[tokio::test]
async fn system_update_rejects_revision_changes() {
    let stale = Fixture::new(requirements("^1.0", "executor"), &[7, 8]);
    let services = super::AdminHarness::new()
        .plugins(stale.clone(), stale)
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("executor"),
        }))
        .build()
        .await;
    assert!(
        services
            .system()
            .perform_update(Some("1.2.0".into()), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn system_update_accepts_new_plugin_contracts_for_empty_disabled_and_enabled_plugins() {
    for instances in [vec![], vec![instance(false)], vec![instance(true)]] {
        let mut fixture = Fixture::new(requirements("^1.0", "executor"), &[7]);
        Arc::get_mut(&mut fixture)
            .expect("exclusive fixture")
            .instances = instances;
        let mut candidate = candidate("models");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&candidate.release_manifest).unwrap();
        manifest["plugin_host"] =
            serde_json::json!({"schema_version": 99, "future_contract": true});
        candidate.release_manifest = serde_json::to_vec(&manifest).unwrap().into();
        let services = super::AdminHarness::new()
            .plugins(fixture.clone(), fixture)
            .system(Arc::new(PreflightingSystem { candidate }))
            .build()
            .await;
        services
            .system()
            .perform_update(Some("1.2.0".into()), None)
            .await
            .expect("plugin confirmation is deferred until restart");
    }
}

#[tokio::test]
async fn system_update_keeps_release_identity_validation_without_plugins() {
    for (field, value) in [
        ("gateway_version", serde_json::json!("1.3.0")),
        ("gateway_git_sha", serde_json::json!("invalid")),
        ("sealed", serde_json::json!(false)),
        ("schema_version", serde_json::json!(99)),
    ] {
        let mut fixture = Fixture::new(requirements("^1.0", "executor"), &[7]);
        Arc::get_mut(&mut fixture).unwrap().instances.clear();
        let mut candidate = candidate("executor");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&candidate.release_manifest).unwrap();
        manifest[field] = value;
        candidate.release_manifest = serde_json::to_vec(&manifest).unwrap().into();
        let services = super::AdminHarness::new()
            .plugins(fixture.clone(), fixture)
            .system(Arc::new(PreflightingSystem { candidate }))
            .build()
            .await;
        assert!(
            services
                .system()
                .perform_update(Some("1.2.0".into()), None)
                .await
                .is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn system_update_still_rechecks_revision_without_plugins() {
    let mut fixture = Fixture::new(requirements("^1.0", "executor"), &[7, 8]);
    Arc::get_mut(&mut fixture).unwrap().instances.clear();
    let services = super::AdminHarness::new()
        .plugins(fixture.clone(), fixture)
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("executor"),
        }))
        .build()
        .await;
    assert!(
        services
            .system()
            .perform_update(Some("1.2.0".into()), None)
            .await
            .is_err()
    );
}

fn instance(enabled: bool) -> PluginInstance {
    PluginInstance {
        id: "enabled-fixture".into(),
        name: "Enabled fixture".into(),
        artifact_sha256: "a".repeat(64),
        enabled,
        trusted_process: true,
        configuration: serde_json::json!({}),
        secrets: BTreeMap::new(),
        bindings: Vec::new(),
        revision: Revision::new(1).expect("revision"),
    }
}

fn requirements(host_version: &str, capability: &str) -> PluginCompatibilityRequirements {
    PluginCompatibilityRequirements {
        host_version: host_version.into(),
        manifest_schema_version: 2,
        protocol_version: 3,
        capabilities: vec![(capability.into(), 1)],
    }
}

fn candidate(capability: &str) -> SystemUpdateCandidate {
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "sealed": true,
        "gateway_version": "1.2.0",
        "gateway_git_sha": "a".repeat(40),
        "plugin_host": {
            "schema_version": 2,
            "manifest_schema_versions": [2],
            "protocol_versions": [3],
            "capabilities": [{ "capability": capability, "versions": [1] }],
        },
        "plugins": [],
    }))
    .expect("manifest");
    SystemUpdateCandidate {
        target_version: "1.2.0".into(),
        release_manifest: manifest.into(),
    }
}

fn context() -> MutationContext {
    MutationContext {
        actor: gateway_admin::model::MutationActor::AdminApiKey,
        request_id: "restart-test".into(),
    }
}

#[tokio::test]
async fn incompatible_restart_requires_exact_confirmation_and_preserves_enabled_plugins() {
    let fixture = Fixture::new(requirements("^1.0", "executor"), &[7]);
    let services = super::AdminHarness::new()
        .plugins(fixture.clone(), fixture.clone())
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("models"),
        }))
        .build()
        .await;
    let plan = services.system().restart_plan().await.unwrap();
    assert_eq!(plan.incompatible_plugins.len(), 1);
    assert_eq!(plan.incompatible_plugins[0].name, "Enabled fixture");
    assert!(
        fixture.disabled.lock().unwrap().is_empty(),
        "检查及取消不能停用插件"
    );
    assert!(services.system().restart(None, &context()).await.is_err());
    for altered in 0..3 {
        let mut changed = plan.clone();
        match altered {
            0 => changed.config_revision += 1,
            1 => changed.release_manifest_sha256 = Some("b".repeat(64)),
            _ => changed.incompatible_plugins.clear(),
        }
        assert!(
            services
                .system()
                .restart(Some(changed), &context())
                .await
                .is_err()
        );
        assert!(fixture.disabled.lock().unwrap().is_empty());
    }
    services
        .system()
        .restart(Some(plan), &context())
        .await
        .unwrap();
    assert!(fixture.disabled.lock().unwrap().is_empty());
    assert_eq!(fixture.next_revision().get(), 7);
    assert_eq!(
        services
            .system()
            .restart_plan()
            .await
            .unwrap()
            .incompatible_plugins
            .len(),
        1
    );
}

#[tokio::test]
async fn compatible_restart_does_not_change_plugin_configuration() {
    let fixture = Fixture::new(requirements("^1.0", "executor"), &[7]);
    let services = super::AdminHarness::new()
        .plugins(fixture.clone(), fixture.clone())
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("executor"),
        }))
        .build()
        .await;
    assert!(
        services
            .system()
            .restart_plan()
            .await
            .unwrap()
            .incompatible_plugins
            .is_empty()
    );
    services.system().restart(None, &context()).await.unwrap();
    assert!(fixture.disabled.lock().unwrap().is_empty());
    assert_eq!(fixture.next_revision().get(), 7);
}

#[tokio::test]
async fn changed_configuration_after_restart_check_requires_a_new_confirmation() {
    let fixture = Fixture::new(requirements("^1.0", "executor"), &[7, 8]);
    let services = super::AdminHarness::new()
        .plugins(fixture.clone(), fixture.clone())
        .system(Arc::new(PreflightingSystem {
            candidate: candidate("models"),
        }))
        .build()
        .await;
    let plan = services.system().restart_plan().await.unwrap();
    assert!(
        services
            .system()
            .restart(Some(plan), &context())
            .await
            .is_err()
    );
    assert!(fixture.disabled.lock().unwrap().is_empty());
}
