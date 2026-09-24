use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;

use super::store::AdminStoreResult;
use crate::model::plugins::distribution::{
    DownloadedPlugin, GithubReleaseQuery, PluginDistributionEgress, PluginRelease,
    PluginSourceBinding, PluginUpdateSource, RemotePluginLocation, SourceCredential,
    SourceCredentialInfo,
};
use crate::model::plugins::instances::{
    PluginInstance, PluginInstanceMutation, PluginInstanceReplacement, PluginInstanceRuntime,
    PluginInstanceSnapshot, PluginVersionConfiguration,
};
use crate::model::plugins::state::{
    ApplyPluginStateMigration, DeletePluginState, PluginStateCommit, PluginStateConfiguration,
    PluginStateMigrationBatch, PluginStateOwner, PluginStateOwnerRequest, PluginStateRecord,
    PluginStateTransition, PluginStateWrite, PutPluginState,
};
use crate::model::{
    AdminError, MutationContext, Revision,
    plugins::{
        InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMutation,
        PluginCompatibilityRequirements, PluginSource,
    },
};
use gateway_core::runtime::extensions::ExtensionSetReference;

/// Admin 准备完整候选并保活到提交后的发布结束；进程状态不能代替持久启用状态。
#[async_trait]
pub trait PluginPreparation: Send + Sync {
    /// 静态判断业务配置是否完整，不启动进程；不完整的安装保留为待配置。
    async fn configuration_ready(
        &self,
        instance: PluginInstance,
        metadata: &crate::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError>;
    async fn validate(
        &self,
        instance: PluginInstance,
    ) -> Result<PluginStateConfiguration, AdminError>;
    /// 候选可使用未来 revision；其依赖的持久化事实必须按当前 source revision 读取。
    /// 本次修改的实例 revision 必须等于候选 config_revision，准备失败时拒绝提交；历史故障可隔离。
    async fn prepare(
        &self,
        source_revision: Revision,
        snapshot: PluginInstanceSnapshot,
    ) -> Result<ExtensionSetReference, AdminError>;
    /// 返回当前进程的只读运行事实；缺省实现让不托管插件进程的组合保持兼容。
    async fn runtime_diagnostics(
        &self,
        _snapshot: &PluginInstanceSnapshot,
        _published_revision: Option<u64>,
        _published: Option<&ExtensionSetReference>,
    ) -> Option<BTreeMap<String, PluginInstanceRuntime>> {
        None
    }
    async fn activate_state(
        &self,
        _prepared: &ExtensionSetReference,
        _instance: &PluginInstance,
    ) -> Result<(), AdminError> {
        Ok(())
    }
    async fn quiesce_instance(
        &self,
        _instance_id: &str,
        _artifact_sha256: &str,
        _revision: Revision,
    ) {
    }
    async fn migrate_state(
        &self,
        _prepared: &ExtensionSetReference,
        _transition: PluginStateTransition,
    ) -> Result<(), AdminError> {
        Err(AdminError::unavailable("插件状态迁移运行时不可用"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStateStoreErrorKind {
    Invalid,
    NotFound,
    Conflict,
    Quota,
    PermissionDenied,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plugin state operation failed")]
pub struct PluginStateStoreError {
    kind: PluginStateStoreErrorKind,
}

impl PluginStateStoreError {
    #[must_use]
    pub const fn new(kind: PluginStateStoreErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> PluginStateStoreErrorKind {
        self.kind
    }
}

pub type PluginStateStoreResult<T> = Result<T, PluginStateStoreError>;

/// 私有状态的窄端口；不暴露 SQL、连接或跨命名空间事务。
#[async_trait]
pub trait PluginStateStore: Send + Sync {
    async fn load_owner(
        &self,
        request: PluginStateOwnerRequest,
    ) -> PluginStateStoreResult<Option<PluginStateOwner>>;
    async fn get(
        &self,
        owner: &PluginStateOwner,
        namespace: &str,
        key: &str,
    ) -> PluginStateStoreResult<Option<PluginStateRecord>>;
    async fn put(
        &self,
        owner: &PluginStateOwner,
        command: PutPluginState,
    ) -> PluginStateStoreResult<PluginStateWrite>;
    async fn delete(
        &self,
        owner: &PluginStateOwner,
        command: DeletePluginState,
    ) -> PluginStateStoreResult<bool>;
    async fn transition_required(
        &self,
        instance_id: &str,
        target: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool>;
    async fn begin_transition(
        &self,
        instance_id: &str,
        expected_instance_revision: Revision,
        artifact_sha256: &str,
        target: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition>;
    async fn migration_batch(
        &self,
        transition_id: &str,
        namespace: &str,
        maximum_records: u32,
    ) -> PluginStateStoreResult<PluginStateMigrationBatch>;
    async fn apply_migration_batch(
        &self,
        command: ApplyPluginStateMigration,
    ) -> PluginStateStoreResult<()>;
    async fn abort_transition(&self, transition_id: &str) -> PluginStateStoreResult<()>;
}

/// Host 拥有联网、重定向、凭据作用域、限流与有界下载，不解释插件包。
#[async_trait]
pub trait PluginDistribution: Send + Sync {
    fn validate_source(&self, source: &PluginUpdateSource) -> Result<(), AdminError>;
    fn validate_credential(&self, credential: &SourceCredential) -> Result<(), AdminError>;
    async fn query_release(
        &self,
        query: GithubReleaseQuery,
        credentials: Vec<SourceCredential>,
        egress: Option<PluginDistributionEgress>,
    ) -> Result<PluginRelease, AdminError>;
    async fn download(
        &self,
        location: RemotePluginLocation,
        credentials: Vec<SourceCredential>,
        egress: Option<PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError>;
}

/// Runtime 只解释插件包格式；安装事务与来源选择归 Admin。
#[async_trait]
pub trait PluginPackageInspector: Send + Sync {
    /// 当前宿主对权限标识的说明，供展示投影使用，不属于不可变制品元数据。
    fn permission_descriptions(
        &self,
        permissions: &[String],
    ) -> Vec<crate::model::plugins::PluginPermissionDescription> {
        permissions
            .iter()
            .map(
                |permission| crate::model::plugins::PluginPermissionDescription {
                    permission: permission.clone(),
                    label: permission.clone(),
                    description: String::new(),
                },
            )
            .collect()
    }

    async fn inspect(
        &self,
        archive: Arc<[u8]>,
        expected_sha256: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError>;

    /// 从已安装且摘要匹配的制品中读取可展示的静态图标，不启动插件实例。
    async fn icon(
        &self,
        _archive: Arc<[u8]>,
        _expected_sha256: String,
        _theme: crate::model::plugins::PluginIconTheme,
    ) -> Result<Option<crate::model::plugins::PluginArtifactIconResource>, AdminError> {
        Err(AdminError::unavailable("插件图标读取不可用"))
    }

    /// 只读取已校验包的静态宿主要求；不得准备或启动插件进程。
    async fn compatibility(
        &self,
        _archive: Arc<[u8]>,
        _expected_sha256: String,
    ) -> Result<PluginCompatibilityRequirements, AdminError> {
        Err(AdminError::unavailable("插件兼容性检查不可用"))
    }
}

#[async_trait]
pub trait PluginStore: Send + Sync {
    /// 只复核精确管理目标的启用、接受事实和版本，不读取实例配置或密钥。
    async fn management_target_is_current(
        &self,
        target: &crate::model::plugins::management::PluginManagementTarget,
    ) -> AdminStoreResult<bool>;
    async fn load_instances(&self) -> AdminStoreResult<PluginInstanceSnapshot>;
    /// 读取对应制品最近一次启用时提交的配置，密钥始终留在服务端。
    async fn load_version_configuration(
        &self,
        _id: &str,
        _digest: &str,
    ) -> AdminStoreResult<Option<PluginVersionConfiguration>> {
        Ok(None)
    }
    /// 只返回有可恢复配置的制品摘要，不读取敏感值。
    async fn configuration_versions(&self, _id: &str) -> AdminStoreResult<Vec<String>> {
        Ok(Vec::new())
    }
    async fn save_instance(
        &self,
        instance: PluginInstance,
        expected_revision: Revision,
        context: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation>;
    async fn save_instance_with_state(
        &self,
        instance: PluginInstance,
        expected_revision: Revision,
        state: PluginStateCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        if !state.configuration.namespaces.is_empty() || state.transition_id.is_some() {
            return Err(super::store::AdminStoreError::new(
                super::store::AdminStoreErrorKind::Unavailable,
                "plugin state",
                "plugin state store is unavailable",
            ));
        }
        self.save_instance(instance, expected_revision, context)
            .await
    }
    /// 保存目标并停用明确确认的配置，必须共享事务和配置版本检查。
    async fn save_instance_replacing(
        &self,
        instance: PluginInstance,
        expected_revision: Revision,
        state: PluginStateCommit,
        replacements: &[PluginInstanceReplacement],
        context: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        if !replacements.is_empty() {
            return Err(super::store::AdminStoreError::new(
                super::store::AdminStoreErrorKind::Unavailable,
                "plugin",
                "atomic plugin configuration switching is unavailable",
            ));
        }
        self.save_instance_with_state(instance, expected_revision, state, context)
            .await
    }

    async fn delete_instance(
        &self,
        id: &str,
        expected_revision: Revision,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>>;
    async fn change_update_source(
        &self,
        binding: PluginSourceBinding,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>>;
    async fn load_source_credential(&self, id: &str) -> AdminStoreResult<SourceCredential>;
    async fn save_source_credential(
        &self,
        credential: SourceCredential,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
    async fn delete_source_credential(
        &self,
        id: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>>;
    async fn load_artifact(&self, digest: &str) -> AdminStoreResult<InspectedPluginArtifact>;
    async fn install_artifact(
        &self,
        artifact: InspectedPluginArtifact,
        source: PluginSource,
        context: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation>;
    /// 接受不可变制品声明的全部能力域；接受事实不随实例设置变化。
    async fn accept_artifact(
        &self,
        digest: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation>;
    /// 删除制品及其无引用下载凭据，最后一个版本同时删除来源规则，清理与审计必须原子提交。
    async fn delete_artifact(
        &self,
        digest: &str,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
}
