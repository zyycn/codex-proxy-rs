use std::{collections::BTreeSet, sync::Arc};

use gateway_core::runtime::SnapshotControl;
use sha2::{Digest as _, Sha256};

use super::super::{map_store_error, publish_committed};
use crate::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactIconResource,
            PluginArtifactMetadata, PluginArtifactMutation, PluginIconTheme, PluginInstallResult,
            PluginSource,
            distribution::VerifiedPluginArtifact,
            instances::{
                ConfigurePluginInstance, PluginCapabilityBinding, PluginFailurePolicy,
                PluginInstance, PluginPermissionGrant,
            },
        },
    },
    ports::plugins::{
        PluginDistribution, PluginPackageInspector, PluginPreparation, PluginStateStore,
        PluginStore,
    },
    ports::proxy::ProxyStore,
};

use super::state::PluginStateService;

/// 远程插件来源访问所需的传输与受管出站代理目录。
pub struct PluginDistributionPorts {
    pub(super) transport: Arc<dyn PluginDistribution>,
    pub(super) proxies: Arc<dyn ProxyStore>,
}

impl PluginDistributionPorts {
    #[must_use]
    pub fn new(transport: Arc<dyn PluginDistribution>, proxies: Arc<dyn ProxyStore>) -> Self {
        Self { transport, proxies }
    }
}

/// 所有安装入口在取得完整包后进入同一校验与事务流程。
pub struct PluginsService {
    pub(super) store: Arc<dyn PluginStore>,
    pub(super) inspector: Arc<dyn PluginPackageInspector>,
    pub(super) distribution: PluginDistributionPorts,
    pub(super) snapshots: Arc<dyn SnapshotControl>,
    pub(super) preparation: Arc<dyn PluginPreparation>,
    pub(super) published: gateway_core::runtime::RuntimeSnapshotHandle,
    pub(super) state: PluginStateService,
}

impl PluginsService {
    pub fn permission_descriptions(
        &self,
        permissions: &[String],
    ) -> Vec<crate::model::plugins::PluginPermissionDescription> {
        self.inspector.permission_descriptions(permissions)
    }

    #[must_use]
    pub fn new(
        store: Arc<dyn PluginStore>,
        inspector: Arc<dyn PluginPackageInspector>,
        distribution: PluginDistributionPorts,
        snapshots: Arc<dyn SnapshotControl>,
        preparation: Arc<dyn PluginPreparation>,
        published: gateway_core::runtime::RuntimeSnapshotHandle,
        state: Arc<dyn PluginStateStore>,
    ) -> Self {
        Self {
            store,
            inspector,
            distribution,
            snapshots,
            preparation,
            published,
            state: PluginStateService::new(state),
        }
    }

    pub async fn list(&self) -> Result<Vec<InstalledPluginArtifact>, AdminError> {
        self.store
            .list_artifacts()
            .await
            .map_err(|error| map_store_error(error, "plugin"))
    }

    pub async fn icon(
        &self,
        digest: &str,
        theme: PluginIconTheme,
    ) -> Result<PluginArtifactIconResource, AdminError> {
        if !valid_digest(digest) {
            return Err(AdminError::invalid("插件制品摘要不合法"));
        }
        let artifact = self
            .store
            .load_artifact(digest)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        if artifact.metadata.icon.is_none() {
            return Err(AdminError::not_found("插件制品未提供图标"));
        }
        self.inspector
            .icon(artifact.archive, digest.to_owned(), theme)
            .await?
            .ok_or_else(|| AdminError::not_found("插件制品未提供图标"))
    }

    pub async fn verify_upload(
        &self,
        archive: Arc<[u8]>,
    ) -> Result<VerifiedPluginArtifact, AdminError> {
        let artifact = self.inspector.inspect(archive, None).await?;
        Ok(VerifiedPluginArtifact {
            metadata: artifact.metadata,
            source: PluginSource::Upload,
        })
    }

    /// 管理上传固定为自定义来源；调用方不能借通用参数声明官方身份。
    pub async fn install_upload(
        &self,
        archive: Arc<[u8]>,
        expected_sha256: Option<String>,
        context: &MutationContext,
    ) -> Result<PluginInstallResult, AdminError> {
        if expected_sha256.is_none() {
            return Err(AdminError::invalid("安装需要已校验的插件包摘要"));
        }
        let artifact = self.inspector.inspect(archive, expected_sha256).await?;
        let mutation = self
            .persist(artifact, PluginSource::Upload, context)
            .await?;
        self.complete_install(mutation, context).await
    }

    /// 接受已导入但尚未安装的制品；官方发行导入也必须经过管理员的这一步。
    pub async fn accept_artifact(
        &self,
        digest: &str,
        context: &MutationContext,
    ) -> Result<PluginInstallResult, AdminError> {
        if !valid_digest(digest) {
            return Err(AdminError::invalid("插件制品摘要不合法"));
        }
        let mutation = self
            .store
            .accept_artifact(digest, context)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), mutation.config_revision).await?;
        self.finish_install(mutation, context).await
    }

    pub(super) async fn complete_install(
        &self,
        mutation: PluginArtifactMutation,
        context: &MutationContext,
    ) -> Result<PluginInstallResult, AdminError> {
        let digest = mutation.artifact.metadata.sha256.clone();
        self.accept_artifact(&digest, context).await
    }

    async fn finish_install(
        &self,
        mut mutation: PluginArtifactMutation,
        context: &MutationContext,
    ) -> Result<PluginInstallResult, AdminError> {
        let metadata = mutation.artifact.metadata.clone();
        let creation_id = default_instance_id(&metadata.plugin_id);
        let configuration = configuration_defaults(&metadata);
        let bindings = default_bindings(&metadata);
        let probe = default_instance(
            &metadata,
            &creation_id,
            configuration.clone(),
            bindings.clone(),
            false,
            mutation.config_revision,
        );
        let configuration_required = !self
            .preparation
            .configuration_ready(probe, &metadata)
            .await?;

        // 其它管理员写入或同插件的并发安装可能推进全局 revision；稳定 creation ID
        // 配合每轮重新读取，既不会复制默认实例，也不会覆盖先完成的另一个版本。
        for _ in 0..3 {
            let snapshot = self
                .store
                .load_instances()
                .await
                .map_err(|error| map_store_error(error, "plugin"))?;
            if let Some((default_instance_id, required)) = self
                .existing_plugin_instance(&snapshot, &metadata, &creation_id)
                .await?
            {
                mutation.config_revision = snapshot.config_revision;
                return Ok(PluginInstallResult {
                    mutation,
                    default_instance_id,
                    configuration_required: required,
                });
            }

            let input = ConfigurePluginInstance {
                replace_instances: Vec::new(),
                creation_id: Some(creation_id.clone()),
                expected_revision: None,
                name: metadata.display_name.clone(),
                artifact_sha256: metadata.sha256.clone(),
                enabled: !configuration_required,
                configuration: configuration.clone(),
                secrets: None,
                bindings: bindings.clone(),
            };
            match self.configure_instance(None, input, context).await {
                Ok(configured) => {
                    mutation.config_revision = configured.config_revision;
                    return Ok(PluginInstallResult {
                        mutation,
                        default_instance_id: Some(configured.instance.id),
                        configuration_required,
                    });
                }
                Err(error) if error.kind() == crate::model::AdminErrorKind::Conflict => {}
                Err(error) => return Err(error),
            }
        }
        Err(AdminError::conflict(
            "插件安装期间配置持续变化，请刷新后重试",
        ))
    }

    async fn existing_plugin_instance(
        &self,
        snapshot: &crate::model::plugins::instances::PluginInstanceSnapshot,
        metadata: &PluginArtifactMetadata,
        default_id: &str,
    ) -> Result<Option<(Option<String>, bool)>, AdminError> {
        let artifacts = self
            .store
            .list_artifacts()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let matching_digests = artifacts
            .into_iter()
            .filter(|artifact| artifact.metadata.plugin_id == metadata.plugin_id)
            .map(|artifact| artifact.metadata.sha256)
            .collect::<BTreeSet<_>>();
        let Some(instance) = snapshot
            .instances
            .iter()
            .find(|instance| matching_digests.contains(&instance.artifact_sha256))
        else {
            return Ok(None);
        };
        if instance.id == default_id && instance.artifact_sha256 == metadata.sha256 {
            let required = !self
                .preparation
                .configuration_ready(instance.clone(), metadata)
                .await?;
            Ok(Some((Some(instance.id.clone()), required)))
        } else {
            Ok(Some((None, false)))
        }
    }

    pub(super) async fn persist(
        &self,
        artifact: InspectedPluginArtifact,
        source: PluginSource,
        context: &MutationContext,
    ) -> Result<PluginArtifactMutation, AdminError> {
        let mutation = self
            .store
            .install_artifact(artifact, source, context)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), mutation.config_revision).await?;
        Ok(mutation)
    }

    pub async fn delete(
        &self,
        digest: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        let revision = self
            .store
            .delete_artifact(digest, context)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), revision).await?;
        Ok(revision)
    }
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn default_instance_id(plugin_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"codex-proxy-rs/plugin-default-instance/v1\0");
    hash.update(plugin_id.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 9562 UUIDv8 保留给应用自定义派生；variant 固定为 RFC 4122。
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

pub(super) fn configuration_defaults(metadata: &PluginArtifactMetadata) -> serde_json::Value {
    let mut configuration = schema_default(&metadata.configuration_schema)
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    merge_property_defaults(&mut configuration, &metadata.configuration_schema);
    if let Some(configuration) = configuration.as_object_mut() {
        for secret in &metadata.secret_fields {
            configuration.remove(secret);
        }
    }
    configuration
}

fn schema_default(schema: &serde_json::Value) -> Option<serde_json::Value> {
    let mut value = if let Some(value) = schema.get("default") {
        value.clone()
    } else {
        let properties = schema.get("properties")?.as_object()?;
        let mut object = serde_json::Map::new();
        for (name, property) in properties {
            if let Some(value) = schema_default(property) {
                object.insert(name.clone(), value);
            }
        }
        if object.is_empty() {
            return None;
        }
        serde_json::Value::Object(object)
    };
    merge_property_defaults(&mut value, schema);
    Some(value)
}

fn merge_property_defaults(value: &mut serde_json::Value, schema: &serde_json::Value) {
    let (Some(value), Some(properties)) = (
        value.as_object_mut(),
        schema
            .get("properties")
            .and_then(serde_json::Value::as_object),
    ) else {
        return;
    };
    for (name, property) in properties {
        if let Some(current) = value.get_mut(name) {
            merge_property_defaults(current, property);
        } else if let Some(default) = schema_default(property) {
            value.insert(name.clone(), default);
        }
    }
}

pub(super) fn default_bindings(metadata: &PluginArtifactMetadata) -> Vec<PluginCapabilityBinding> {
    metadata
        .contributes
        .iter()
        .filter(|(capability, _)| {
            !matches!(
                capability.as_str(),
                "frontend_authentication" | "management" | "command_line"
            )
        })
        .flat_map(|(_, contribution)| {
            contribution
                .stages
                .iter()
                .map(|stage| PluginCapabilityBinding {
                    contribution: contribution.id.clone(),
                    stage: stage.clone(),
                    order: 0,
                    failure_policy: if stage == "observation" {
                        PluginFailurePolicy::Observe
                    } else {
                        PluginFailurePolicy::Reject
                    },
                    client_key_ids: Vec::new(),
                    account_group_ids: Vec::new(),
                    provider_ids: Vec::new(),
                    models: Vec::new(),
                    identity_bindings: Vec::new(),
                })
        })
        .collect()
}

fn default_instance(
    metadata: &PluginArtifactMetadata,
    id: &str,
    configuration: serde_json::Value,
    bindings: Vec<PluginCapabilityBinding>,
    enabled: bool,
    revision: Revision,
) -> PluginInstance {
    PluginInstance {
        id: id.to_owned(),
        name: metadata.display_name.clone(),
        artifact_sha256: metadata.sha256.clone(),
        enabled,
        trusted_process: true,
        configuration,
        secrets: Default::default(),
        grants: metadata
            .requested_permissions
            .iter()
            .cloned()
            .map(|permission| PluginPermissionGrant { permission })
            .collect(),
        bindings,
        revision,
    }
}
