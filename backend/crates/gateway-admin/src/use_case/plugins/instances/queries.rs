//! 插件实例列表与静态包检查的只读投影，不从发布事实推断进程状态

use std::collections::BTreeMap;

use super::{PackageStatus, PluginsService};
use crate::{
    model::{
        AdminError, AdminErrorKind,
        plugins::instances::{
            PluginInstanceRuntime, PluginInstanceRuntimeStatus, PluginInstanceView,
        },
    },
    use_case::map_store_error,
};

impl PluginsService {
    /// 只校验固定包体，不启动插件；停用实例也需区分兼容性提醒与加载错误
    pub(in crate::use_case::plugins) async fn package_status(
        &self,
        digest: &str,
    ) -> Result<PackageStatus, AdminError> {
        // 运行宿主和摘要固定，缓存纯静态结论，避免管理页轮询反复解包
        // 只缓存有限数量的确定结果，暂时性错误仍可重试
        if let Some(status) = self.compatibility.lock().await.get(digest).cloned() {
            return Ok(status);
        }
        let artifact = self
            .store
            .load_artifact(digest)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let status = match self
            .inspector
            .compatibility_warning(artifact.archive, digest.to_owned())
            .await
        {
            Ok(warning) => PackageStatus {
                warning,
                load_error: None,
            },
            Err(error) if error.kind() == AdminErrorKind::Invalid => PackageStatus {
                warning: None,
                load_error: Some(error.message().to_owned()),
            },
            Err(error) => return Err(error),
        };
        let mut cache = self.compatibility.lock().await;
        if cache.len() < 128 {
            cache.insert(digest.to_owned(), status.clone());
        }
        Ok(status)
    }

    pub async fn instances(&self) -> Result<Vec<PluginInstanceView>, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        let published = self.published.snapshot_for_diagnostics();
        let published_revision = published.as_ref().map(|snapshot| snapshot.revision().get());
        let mut diagnostics = self
            .preparation
            .runtime_diagnostics(
                &snapshot,
                published_revision,
                published
                    .as_ref()
                    .and_then(|snapshot| snapshot.extensions()),
            )
            .await
            .unwrap_or_default();
        let artifacts = self
            .list()
            .await?
            .into_iter()
            .map(|artifact| (artifact.metadata.sha256.clone(), artifact.metadata))
            .collect::<BTreeMap<_, _>>();
        let mut views = Vec::with_capacity(snapshot.instances.len());
        for instance in snapshot.instances {
            let metadata = artifacts
                .get(&instance.artifact_sha256)
                .ok_or_else(|| AdminError::not_found("插件制品不存在"))?;
            let package_status = self.package_status(&instance.artifact_sha256).await?;
            let configuration_required = package_status.load_error.is_none()
                && !self
                    .preparation
                    .configuration_ready(instance.clone(), metadata)
                    .await?;
            let runtime =
                diagnostics
                    .remove(&instance.id)
                    .unwrap_or_else(|| PluginInstanceRuntime {
                        status: if !instance.enabled {
                            PluginInstanceRuntimeStatus::Disabled
                        } else {
                            PluginInstanceRuntimeStatus::Unknown
                        },
                        actual_revision: None,
                        actual_artifact_sha256: None,
                        failure: None,
                        draining_revisions: Vec::new(),
                        load: None,
                    });
            views.push(PluginInstanceView {
                api_deprecations: if package_status.load_error.is_none() {
                    self.inspector.api_deprecations(metadata)?
                } else {
                    Vec::new()
                },
                configuration_required,
                compatibility_warning: package_status.warning,
                load_error: package_status.load_error,
                running: runtime.status == PluginInstanceRuntimeStatus::Running,
                published_revision,
                runtime,
                instance,
            });
        }
        Ok(views)
    }
}
