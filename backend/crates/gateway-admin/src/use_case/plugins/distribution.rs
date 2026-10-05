//! 插件更新来源与下载凭据管理，以及远程制品验证和安装编排

use std::collections::BTreeSet;

use super::PluginsService;
use crate::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, PluginInstallResult, PluginSource,
            distribution::{
                GithubReleaseQuery, PluginDistributionEgress, PluginRelease, PluginSourceBinding,
                PluginUpdateCheck, PluginUpdatePolicy, PluginUpdateSource, RemotePluginInstall,
                RemotePluginLocation, RemotePluginVerify, SourceAuthentication, SourceCredential,
                SourceCredentialInfo, VerifiedPluginArtifact,
            },
        },
    },
    ports::store::AdminStoreErrorKind,
    use_case::{map_store_error, publish_committed},
};

impl PluginsService {
    pub async fn update_sources(
        &self,
    ) -> Result<Vec<crate::model::plugins::distribution::PluginSourceBinding>, AdminError> {
        self.store
            .list_update_sources()
            .await
            .map_err(|e| map_store_error(e, "plugin"))
    }

    pub async fn change_update_source(
        &self,
        mut binding: crate::model::plugins::distribution::PluginSourceBinding,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        if matches!(binding.source, PluginUpdateSource::Builtin) {
            return Err(AdminError::invalid("官方来源只由发行清单确认"));
        }
        if let PluginUpdateSource::Github { repository } = &mut binding.source {
            repository.make_ascii_lowercase();
        }
        self.distribution
            .transport
            .validate_source(&binding.source)?;
        validate_update_policy(&binding)?;
        validate_source_egress(&binding)?;
        self.resolve_egress(binding.outbound_proxy_id.as_deref())
            .await?;
        let revision = self
            .store
            .change_update_source(binding, context)
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), revision).await?;
        Ok(revision)
    }

    pub async fn check_update(
        &self,
        plugin_id: &str,
        credential_ids: &[String],
    ) -> Result<PluginUpdateCheck, AdminError> {
        let binding = self
            .update_sources()
            .await?
            .into_iter()
            .find(|binding| binding.plugin_id == plugin_id)
            .ok_or_else(|| AdminError::not_found("插件更新来源不存在"))?;
        validate_update_policy(&binding)?;
        let PluginUpdateSource::Github { repository } = &binding.source else {
            return Err(AdminError::invalid(
                "该来源不提供 Release 更新查询，请显式选择并安装插件包",
            ));
        };
        let (tag, allow_prerelease) = match &binding.policy {
            PluginUpdatePolicy::Manual {} => {
                return Err(AdminError::invalid("请先为该来源设置更新检查策略"));
            }
            PluginUpdatePolicy::Stable {} => (None, false),
            PluginUpdatePolicy::Pinned {
                tag,
                allow_prerelease,
            } => (Some(tag.clone()), *allow_prerelease),
        };
        let release = self
            .query_release(
                GithubReleaseQuery {
                    repository: repository.clone(),
                    tag,
                    allow_prerelease,
                },
                credential_ids,
                binding.outbound_proxy_id.as_deref(),
            )
            .await?;
        // 查询期间来源或策略被修改时返回冲突，不把旧来源的候选交给下一步安装
        if !self.update_sources().await?.contains(&binding) {
            return Err(AdminError::conflict("插件更新来源已变更，请重新检查"));
        }
        Ok(PluginUpdateCheck { binding, release })
    }
    pub async fn source_credentials(&self) -> Result<Vec<SourceCredentialInfo>, AdminError> {
        self.store
            .list_source_credentials()
            .await
            .map_err(|e| map_store_error(e, "plugin"))
    }

    pub async fn create_source_credential(
        &self,
        mut info: SourceCredentialInfo,
        authentication: SourceAuthentication,
        context: &MutationContext,
    ) -> Result<SourceCredentialInfo, AdminError> {
        info.id = uuid::Uuid::now_v7().to_string();
        let credential = SourceCredential {
            info: info.clone(),
            authentication,
        };
        self.distribution
            .transport
            .validate_credential(&credential)?;
        let revision = self
            .store
            .save_source_credential(credential, context)
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), revision).await?;
        Ok(info)
    }

    pub async fn delete_source_credential(
        &self,
        id: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        let revision = self
            .store
            .delete_source_credential(id, context)
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), revision).await?;
        Ok(revision)
    }

    pub async fn query_release(
        &self,
        query: GithubReleaseQuery,
        credential_ids: &[String],
        outbound_proxy_id: Option<&str>,
    ) -> Result<PluginRelease, AdminError> {
        let egress = self.resolve_egress(outbound_proxy_id).await?;
        let release = self
            .distribution
            .transport
            .query_release(
                query,
                self.credentials(credential_ids).await?,
                egress.clone(),
            )
            .await?;
        self.ensure_egress_current(egress.as_ref()).await?;
        Ok(release)
    }

    pub async fn install_remote(
        &self,
        input: RemotePluginInstall,
        context: &MutationContext,
    ) -> Result<PluginInstallResult, AdminError> {
        if !crate::model::plugins::valid_plugin_id(&input.plugin_id)
            || input.version.is_empty()
            || input.version.len() > 128
        {
            return Err(AdminError::invalid("安装需要已解析的插件 ID 与版本"));
        }
        let digest = match &input.location {
            RemotePluginLocation::Url { sha256, .. }
            | RemotePluginLocation::Github { sha256, .. } => sha256,
        };
        if digest.is_none() {
            return Err(AdminError::invalid("安装需要已校验的插件包摘要"));
        }
        let (artifact, source) = self
            .inspect_remote(RemotePluginVerify {
                location: input.location,
                credential_ids: input.credential_ids,
                outbound_proxy_id: input.outbound_proxy_id,
                expected_plugin_id: Some(input.plugin_id),
            })
            .await?;
        if artifact.metadata.version != input.version {
            return Err(AdminError::invalid("插件清单版本与安装选择不符"));
        }
        let mutation = self.persist(artifact, source, context).await?;
        self.complete_install(mutation, context).await
    }

    pub async fn verify_remote(
        &self,
        input: RemotePluginVerify,
    ) -> Result<VerifiedPluginArtifact, AdminError> {
        let (artifact, source) = self.inspect_remote(input).await?;
        Ok(VerifiedPluginArtifact {
            metadata: artifact.metadata,
            source,
        })
    }

    async fn inspect_remote(
        &self,
        input: RemotePluginVerify,
    ) -> Result<(InspectedPluginArtifact, PluginSource), AdminError> {
        if input
            .expected_plugin_id
            .as_deref()
            .is_some_and(|id| !crate::model::plugins::valid_plugin_id(id))
        {
            return Err(AdminError::invalid("预期插件 ID 不合法"));
        }
        let source = match &input.location {
            RemotePluginLocation::Url { url, .. } => PluginUpdateSource::Url { url: url.clone() },
            RemotePluginLocation::Github { repository, .. } => PluginUpdateSource::Github {
                repository: repository.to_ascii_lowercase(),
            },
        };
        let outbound_proxy_id = input.outbound_proxy_id.clone();
        self.distribution.transport.validate_source(&source)?;
        let bindings = self.update_sources().await?;
        let expected_binding = bindings
            .iter()
            .find(|binding| Some(&binding.plugin_id) == input.expected_plugin_id.as_ref());
        if expected_binding.is_some_and(|binding| {
            binding.source != source || binding.outbound_proxy_id != outbound_proxy_id
        }) {
            return Err(AdminError::conflict(
                "插件来源与已保存配置不符，请先显式修改来源",
            ));
        }
        let egress = self.resolve_egress(outbound_proxy_id.as_deref()).await?;
        let download = self
            .distribution
            .transport
            .download(
                input.location,
                self.credentials(&input.credential_ids).await?,
                egress.clone(),
            )
            .await?;
        self.ensure_egress_current(egress.as_ref()).await?;
        let artifact = self
            .inspector
            .inspect(download.archive, Some(download.sha256))
            .await?;
        if input
            .expected_plugin_id
            .as_ref()
            .is_some_and(|id| *id != artifact.metadata.plugin_id)
        {
            return Err(AdminError::invalid("插件清单身份与安装选择不符"));
        }
        // 首次解析前身份未知，按包内身份复核下载前的来源绑定
        let binding = bindings
            .iter()
            .find(|binding| binding.plugin_id == artifact.metadata.plugin_id);
        if binding.is_some_and(|binding| {
            binding.source != source || binding.outbound_proxy_id != outbound_proxy_id
        }) {
            return Err(AdminError::conflict(
                "插件来源与已保存配置不符，请先显式修改来源",
            ));
        }
        if PluginUpdateSource::from(&download.source) != source {
            return Err(AdminError::invalid("下载来源与校验选择不符"));
        }
        if download.source.outbound_proxy() != egress.as_ref().map(|egress| &egress.source) {
            return Err(AdminError::invalid("下载出站身份与安装选择不符"));
        }
        let current_binding = self
            .update_sources()
            .await?
            .into_iter()
            .find(|binding| binding.plugin_id == artifact.metadata.plugin_id);
        if current_binding.as_ref() != binding {
            return Err(AdminError::conflict("插件更新来源已变更，请重新校验"));
        }
        Ok((artifact, download.source))
    }

    async fn resolve_egress(
        &self,
        outbound_proxy_id: Option<&str>,
    ) -> Result<Option<PluginDistributionEgress>, AdminError> {
        let Some(id) = outbound_proxy_id else {
            return Ok(None);
        };
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(AdminError::invalid("插件来源出站代理不合法"));
        }
        let record = self
            .distribution
            .proxies
            .get(id)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        Ok(Some(PluginDistributionEgress::new(
            record.id,
            record.revision,
            record.proxy,
        )))
    }

    async fn ensure_egress_current(
        &self,
        egress: Option<&PluginDistributionEgress>,
    ) -> Result<(), AdminError> {
        let Some(egress) = egress else {
            return Ok(());
        };
        let current = match self.distribution.proxies.get(&egress.source.id).await {
            Ok(current) => current,
            Err(error) if error.kind() == AdminStoreErrorKind::NotFound => {
                return Err(AdminError::conflict("插件来源出站代理已变更，请重新校验"));
            }
            Err(error) => return Err(map_store_error(error, "proxy")),
        };
        if current.revision.get() != egress.source.revision || current.proxy != egress.proxy {
            return Err(AdminError::conflict("插件来源出站代理已变更，请重新校验"));
        }
        Ok(())
    }

    async fn credentials(&self, ids: &[String]) -> Result<Vec<SourceCredential>, AdminError> {
        if ids.len() > 8 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err(AdminError::invalid("下载凭据不能重复，且最多选择 8 个"));
        }
        let mut credentials = Vec::with_capacity(ids.len());
        for id in ids {
            credentials.push(
                self.store
                    .load_source_credential(id)
                    .await
                    .map_err(|e| map_store_error(e, "plugin"))?,
            );
        }
        Ok(credentials)
    }
}

fn validate_update_policy(binding: &PluginSourceBinding) -> Result<(), AdminError> {
    if !matches!(binding.source, PluginUpdateSource::Github { .. })
        && !matches!(binding.policy, PluginUpdatePolicy::Manual {})
    {
        return Err(AdminError::invalid(
            "只有 GitHub 来源支持 Release 更新检查策略",
        ));
    }
    if let PluginUpdatePolicy::Pinned { tag, .. } = &binding.policy
        && (tag.is_empty()
            || tag.len() > 128
            || tag.chars().any(char::is_control)
            || tag.trim() != tag)
    {
        return Err(AdminError::invalid("固定 Release tag 不合法"));
    }
    Ok(())
}

fn validate_source_egress(binding: &PluginSourceBinding) -> Result<(), AdminError> {
    if binding.outbound_proxy_id.is_some()
        && matches!(
            binding.source,
            PluginUpdateSource::Builtin | PluginUpdateSource::Upload
        )
    {
        return Err(AdminError::invalid("本地或官方来源不能配置出站代理"));
    }
    Ok(())
}
