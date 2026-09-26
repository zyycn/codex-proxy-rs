use async_trait::async_trait;
use gateway_admin::model::plugins::distribution::{
    DownloadedPlugin, GithubReleaseQuery, PluginRelease, RemotePluginLocation, SourceCredential,
    SourceCredentialInfo,
};
use gateway_admin::model::plugins::distribution::{PluginSourceBinding, PluginUpdateSource};
use gateway_admin::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMutation, PluginSource,
            state::{
                ApplyPluginStateMigration, DeletePluginState, PluginStateConfiguration,
                PluginStateMigrationBatch, PluginStateOwner, PluginStateOwnerRequest,
                PluginStateRecord, PluginStateTransition, PluginStateWrite, PutPluginState,
            },
        },
    },
    ports::{
        plugins::{
            PluginPackageInspector, PluginStateStore, PluginStateStoreError,
            PluginStateStoreErrorKind, PluginStateStoreResult, PluginStore,
        },
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use std::sync::Arc;

mod artifacts;
mod distribution;
mod instances;
mod official;

pub(super) struct TestPluginPorts;

#[async_trait]
impl gateway_admin::ports::proxy::ProxyStore for TestPluginPorts {
    async fn reserve_import(
        &self,
        _: &str,
    ) -> AdminStoreResult<gateway_admin::ports::proxy::ProxyImportReservation> {
        Err(unavailable())
    }

    async fn list(
        &self,
        _: gateway_admin::model::proxies::ProxyListQuery,
    ) -> AdminStoreResult<gateway_admin::model::proxies::ProxyPage> {
        Err(unavailable())
    }

    async fn list_accounts(
        &self,
        _: gateway_admin::model::proxies::ProxyAccountListQuery,
    ) -> AdminStoreResult<gateway_admin::model::proxies::ProxyAccountPage> {
        Err(unavailable())
    }

    async fn get(&self, _: &str) -> AdminStoreResult<gateway_admin::model::proxies::ProxyRecord> {
        Err(unavailable())
    }

    async fn remove_account(
        &self,
        _: &str,
        _: &gateway_core::account::ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }

    async fn create(
        &self,
        _: gateway_admin::model::proxies::NewProxy,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::proxies::ProxyMutation> {
        Err(unavailable())
    }

    async fn update(
        &self,
        _: gateway_admin::model::proxies::UpdateProxy,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::proxies::ProxyMutation> {
        Err(unavailable())
    }

    async fn delete(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }

    async fn record_test(
        &self,
        _: &str,
        _: Revision,
        _: gateway_admin::model::proxies::ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::proxies::ProxyMutation> {
        Err(unavailable())
    }
}

#[async_trait]
impl gateway_admin::ports::plugin_management::PluginManagement for TestPluginPorts {
    async fn authorize_models(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> Result<(), AdminError> {
        Err(AdminError::not_found("unused plugin fixture"))
    }

    async fn start_callback(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: gateway_admin::model::plugins::management::StartPluginManagementCallback,
        _: &gateway_admin::model::auth::AdminRequestContext,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementCallbackTicket, AdminError>
    {
        Err(AdminError::not_found("unused plugin fixture"))
    }
    async fn callback(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: &str,
        _: gateway_admin::model::plugins::management::PluginManagementRequest,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        Err(AdminError::not_found("unused plugin fixture"))
    }
    async fn views(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
    ) -> Result<Vec<gateway_admin::model::plugins::management::PluginManagementView>, AdminError>
    {
        Ok(Vec::new())
    }
    async fn resource(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: &str,
        _: bool,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        Err(AdminError::not_found("unused plugin fixture"))
    }
    async fn handle(
        &self,
        _: &gateway_core::runtime::extensions::ExtensionSetReference,
        _: &gateway_admin::model::plugins::management::PluginManagementTarget,
        _: gateway_admin::model::plugins::management::PluginManagementRequest,
    ) -> Result<gateway_admin::model::plugins::management::PluginManagementResponse, AdminError>
    {
        Err(AdminError::not_found("unused plugin fixture"))
    }
}

#[async_trait]
impl gateway_admin::ports::plugins::PluginPreparation for TestPluginPorts {
    async fn configuration_ready(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstance,
        _: &gateway_admin::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }

    async fn validate(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstance,
    ) -> Result<gateway_admin::model::plugins::state::PluginStateConfiguration, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
    async fn prepare(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstanceSnapshot,
    ) -> Result<gateway_core::runtime::extensions::ExtensionSetReference, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
}

fn unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin",
        "unused plugin fixture",
    )
}

fn state_unavailable<T>() -> PluginStateStoreResult<T> {
    Err(PluginStateStoreError::new(
        PluginStateStoreErrorKind::Unavailable,
    ))
}

#[async_trait]
impl PluginStateStore for TestPluginPorts {
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
        _: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool> {
        state_unavailable()
    }
    async fn begin_transition(
        &self,
        _: &str,
        _: Revision,
        _: &str,
        _: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition> {
        state_unavailable()
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
        state_unavailable()
    }
}

#[async_trait]
impl PluginStore for TestPluginPorts {
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
    async fn load_instances(
        &self,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceSnapshot> {
        Err(unavailable())
    }
    async fn save_instance(
        &self,
        _: gateway_admin::model::plugins::instances::PluginInstance,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<gateway_admin::model::plugins::instances::PluginInstanceMutation> {
        Err(unavailable())
    }
    async fn delete_instance(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }
    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        Ok(Vec::new())
    }
    async fn change_update_source(
        &self,
        _: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }

    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        Ok(Vec::new())
    }
    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        Err(unavailable())
    }
    async fn save_source_credential(
        &self,
        _: SourceCredential,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }
    async fn delete_source_credential(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }

    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        Ok(Vec::new())
    }
    async fn load_artifact(&self, _: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        Err(unavailable())
    }
    async fn install_artifact(
        &self,
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(unavailable())
    }
    async fn accept_artifact(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(unavailable())
    }
    async fn delete_artifact(&self, _: &str, _: &MutationContext) -> AdminStoreResult<Revision> {
        Err(unavailable())
    }
}

#[async_trait]
impl PluginPackageInspector for TestPluginPorts {
    async fn inspect(
        &self,
        _: Arc<[u8]>,
        _: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
}

#[async_trait]
impl gateway_admin::ports::plugins::PluginDistribution for TestPluginPorts {
    fn validate_source(&self, _: &PluginUpdateSource) -> Result<(), AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }

    fn validate_credential(&self, _: &SourceCredential) -> Result<(), AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
    async fn query_release(
        &self,
        _: GithubReleaseQuery,
        _: Vec<SourceCredential>,
        _: Option<gateway_admin::model::plugins::distribution::PluginDistributionEgress>,
    ) -> Result<PluginRelease, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
    async fn download(
        &self,
        _: RemotePluginLocation,
        _: Vec<SourceCredential>,
        _: Option<gateway_admin::model::plugins::distribution::PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError> {
        Err(AdminError::invalid("unused plugin fixture"))
    }
}

#[async_trait::async_trait]
impl gateway_admin::ports::plugin_resources::PluginResourceStore for TestPluginPorts {
    async fn ensure_group(
        &self,
        _: &gateway_admin::model::plugin_resources::PluginResourceOwner,
        _: String,
        _: gateway_admin::model::account_groups::NewAccountGroup,
        _: &gateway_admin::model::MutationContext,
    ) -> gateway_admin::ports::store::AdminStoreResult<
        gateway_admin::model::plugin_resources::ResourceMutation<
            gateway_admin::model::plugin_resources::ManagedResource,
        >,
    > {
        unreachable!("resource port is not used by this fixture")
    }
    async fn ensure_key(
        &self,
        _: &gateway_admin::model::plugin_resources::PluginResourceOwner,
        _: String,
        _: Vec<String>,
        _: gateway_admin::model::client_keys::NewClientKey,
        _: &gateway_admin::model::MutationContext,
    ) -> gateway_admin::ports::store::AdminStoreResult<
        gateway_admin::model::plugin_resources::ResourceMutation<
            gateway_admin::model::plugin_resources::ManagedResource,
        >,
    > {
        unreachable!("resource port is not used by this fixture")
    }
    async fn change_members(
        &self,
        _: &gateway_admin::model::plugin_resources::PluginResourceOwner,
        _: gateway_admin::model::plugin_resources::GroupMembersChange,
        _: &gateway_admin::model::MutationContext,
    ) -> gateway_admin::ports::store::AdminStoreResult<
        gateway_admin::model::plugin_resources::ResourceMutation<
            gateway_admin::model::plugin_resources::GroupMembersChanged,
        >,
    > {
        unreachable!("resource port is not used by this fixture")
    }
}
