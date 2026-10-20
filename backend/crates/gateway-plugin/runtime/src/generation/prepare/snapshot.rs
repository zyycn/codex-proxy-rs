//! 校验实例配置并准备可发布的完整能力集合

use super::{
    PluginRuntime,
    diagnostics::{PreparationDiagnostic, runtime_failure_from_admin},
    set::{PreparedContributions, PreparedSet, fingerprint, instance_fingerprint},
};
use crate::ValidatedPackage;
use async_trait::async_trait;
use futures::{StreamExt as _, future::BoxFuture};
use gateway_admin::{
    model::{
        AdminError, Revision,
        plugins::{
            instances::{PluginInstance, PluginInstanceSnapshot},
            state::PluginStateConfiguration,
        },
    },
    ports::plugins::PluginPreparation,
};
use gateway_core::{
    routing::ConfigRevision,
    routing::extensions::{
        ExtensionPreparationError, ExtensionPreparationPort, ExtensionSetId, ExtensionSetReference,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak, atomic::Ordering},
    time::Duration,
};

impl PluginRuntime {
    pub(super) async fn prepare_snapshot(
        &self,
        snapshot: PluginInstanceSnapshot,
        required_revision: Option<Revision>,
        request_body_limit: Option<usize>,
    ) -> Result<ExtensionSetReference, AdminError> {
        let target_revision = snapshot.config_revision.get();
        self.record_preparation(target_revision, PreparationDiagnostic::Preparing);
        let result = self
            .prepare_inner(snapshot, required_revision, request_body_limit)
            .await;
        match &result {
            Ok(reference) => self.record_preparation(
                target_revision,
                PreparationDiagnostic::Prepared {
                    set_id: reference.id().as_str().to_owned(),
                },
            ),
            Err(error) => self.record_preparation(
                target_revision,
                PreparationDiagnostic::Failed(runtime_failure_from_admin(error)),
            ),
        }
        result
    }

    async fn prepare_inner(
        &self,
        snapshot: PluginInstanceSnapshot,
        required_revision: Option<Revision>,
        request_body_limit: Option<usize>,
    ) -> Result<ExtensionSetReference, AdminError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AdminError::unavailable("插件运行时正在关闭"));
        }
        if snapshot.instances.len() > 64 {
            return Err(AdminError::invalid("最多配置 64 个插件实例"));
        }
        let fingerprint = fingerprint(&snapshot)?;
        // 准备串行化与弱索引分锁；诊断和已发布请求不会等待外部 I/O
        let _prepare = self.prepare_lock.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AdminError::unavailable("插件运行时正在关闭"));
        }
        let existing = {
            let mut prepared = self.prepared.lock().await;
            prepared.retain(|_, set| set.strong_count() > 0);
            prepared
                .get(&fingerprint)
                .and_then(Weak::upgrade)
                .filter(|set| set.sessions_ready())
        };
        if let Some(set) = existing {
            if snapshot.instances.iter().any(|instance| {
                required_revision == Some(instance.revision)
                    && set.failures.contains_key(&instance.id)
            }) {
                return Err(AdminError::unavailable("插件启动失败，请重新启用后重试"));
            }
            return Ok(ExtensionSetReference::new(set.id.clone(), set));
        }
        self.prepared_instances
            .lock()
            .await
            .retain(|_, instance| instance.strong_count() > 0);
        let mut contributions = PreparedContributions::default();
        let mut sessions = Vec::new();
        let mut failures = BTreeMap::new();
        let mut identities = BTreeSet::new();
        let mut instances = Vec::new();
        for instance in snapshot
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if !identities.insert(instance.id.clone()) {
                return Err(AdminError::invalid("插件实例 ID 重复"));
            }
            let identity = instance_fingerprint(instance)?;
            instances.push((instance.clone(), identity));
        }
        // 沿用包校验的并发容量；按快照顺序消费结果，能力组合顺序不随启动快慢变化
        let mut preparing = futures::stream::iter(instances.into_iter().map(
            |(instance, identity)| async move {
                let reusable = {
                    let instances = self.prepared_instances.lock().await;
                    instances
                        .get(&identity)
                        .and_then(Weak::upgrade)
                        .filter(|instance| instance.session.is_ready())
                };
                let result = if let Some(instance) = reusable {
                    Ok(instance)
                } else {
                    tokio::time::timeout(
                        // 沿用准备阶段原有的 30 秒预算；打包校验与能力恢复也在预算内
                        Duration::from_secs(30),
                        self.prepare_instance(
                            instance.clone(),
                            snapshot.config_revision,
                            request_body_limit,
                        ),
                    )
                    .await
                    .unwrap_or_else(|_| Err(AdminError::unavailable("插件启动超时")))
                };
                (instance, identity, result)
            },
        ))
        .buffered(super::runtime::MAXIMUM_PARALLEL_PREPARATIONS);
        while let Some((instance, identity, result)) = preparing.next().await {
            match result {
                Ok(candidate) => {
                    self.prepared_instances
                        .lock()
                        .await
                        .insert(identity, Arc::downgrade(&candidate));
                    contributions.append(candidate.contributions.clone());
                    sessions.push(candidate);
                }
                Err(error) if required_revision == Some(instance.revision) => return Err(error),
                Err(error) => {
                    // 目录没有拒绝请求用的 binding，不能在恢复失败时悄悄撤销仍启用的别名
                    // 不复制其他候选的注册结果；让发布事务保留调用方持有的旧有效快照
                    let has_retained_catalog = self
                        .prepared
                        .lock()
                        .await
                        .values()
                        .filter_map(Weak::upgrade)
                        .any(|set| {
                            set.model_aliases
                                .iter()
                                .any(|alias| alias.owner == instance.id)
                        });
                    if has_retained_catalog {
                        return Err(AdminError::unavailable(
                            "已启用的模型目录插件未就绪，请修复或停用后重试",
                        ));
                    }
                    // 恢复失败只隔离该实例；绑定的拒绝策略仍保留，不能绕过认证或必需处理
                    contributions
                        .policy_entries
                        .extend(crate::adapter::policy::unavailable_entries(&instance)?);
                    contributions
                        .middleware_entries
                        .extend(crate::adapter::middleware::unavailable_entries(&instance)?);
                    contributions.upstream_entries.extend(
                        crate::adapter::upstream_adapter::unavailable_entries(&instance)?,
                    );
                    if let Some(entry) =
                        crate::adapter::frontend_authentication::unavailable_entry(&instance)
                    {
                        contributions.authentication_entries.push(entry);
                    }
                    failures.insert(instance.id.clone(), runtime_failure_from_admin(&error));
                }
            }
        }
        let PreparedContributions {
            observer_entries,
            commands,
            management,
            policy_entries,
            middleware_entries,
            upstream_entries,
            authentication_entries,
            model_aliases,
        } = contributions;
        let id = ExtensionSetId::new(uuid::Uuid::new_v4().to_string())
            .map_err(|_| AdminError::internal("插件集合 ID 无效"))?;
        let observers = crate::adapter::observer::PluginObserverPlan::compile(
            observer_entries,
            self.config
                .rpc_limits
                .maximum_calls
                .min(self.config.rpc_limits.maximum_executing_calls),
            self.config.rpc_limits.maximum_call_timeout,
            self.config.rpc_limits.maximum_buffered_body_bytes,
        )
        .map(|plan| {
            self.observers
                .register(id.clone(), plan)
                .map_err(|_| AdminError::invalid("插件观察计划注册冲突"))
        })
        .transpose()?;
        let policies = crate::adapter::policy::PluginRequestPolicyPlan::compile(
            policy_entries,
            self.config.rpc_limits.maximum_call_timeout,
        )?
        .map(|plan| {
            self.policies
                .register(id.clone(), plan)
                .map_err(|_| AdminError::invalid("插件请求策略计划注册冲突"))
        })
        .transpose()?;
        let middleware = crate::adapter::middleware::PluginMiddlewarePlan::compile(
            middleware_entries,
            self.config.rpc_limits.maximum_call_timeout,
        )
        .map(|plan| plan as Arc<dyn gateway_core::engine::middleware::MiddlewarePlan>);
        let upstream_adapters =
            crate::adapter::upstream_adapter::PluginUpstreamAdapterPlan::compile(upstream_entries);
        let execution = if middleware.is_some() || upstream_adapters.is_some() {
            Some(
                self.execution
                    .register(
                        id.clone(),
                        Arc::new(gateway_core::engine::extensions::ExecutionExtensionPlans {
                            middleware,
                            upstream_adapters,
                        }),
                    )
                    .map_err(|_| AdminError::invalid("插件执行计划注册冲突"))?,
            )
        } else {
            None
        };
        let authentication =
            crate::adapter::frontend_authentication::PluginFrontendAuthenticationPlan::compile(
                authentication_entries,
                self.config.rpc_limits.maximum_call_timeout,
            )?
            .map(|plan| {
                self.authentications
                    .register(id.clone(), plan)
                    .map_err(|_| AdminError::invalid("插件入口认证计划注册冲突"))
            })
            .transpose()?;
        let set = Arc::new(PreparedSet {
            id,
            _observers: observers,
            _policies: policies,
            _execution: execution,
            _authentication: authentication,
            sessions,
            commands,
            management,
            failures,
            model_aliases,
            shutting_down: self.shutting_down.clone(),
        });
        self.prepared
            .lock()
            .await
            .insert(fingerprint, Arc::downgrade(&set));
        Ok(ExtensionSetReference::new(set.id.clone(), set))
    }
    pub(super) async fn prepared_set(
        &self,
        reference: &ExtensionSetReference,
    ) -> Result<Arc<PreparedSet>, AdminError> {
        self.prepared
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
            .find(|set| set.id == *reference.id())
            .ok_or_else(|| AdminError::conflict("插件候选已过期，请重试"))
    }
}
#[async_trait]
impl PluginPreparation for PluginRuntime {
    async fn configuration_ready(
        &self,
        instance: PluginInstance,
        metadata: &gateway_admin::model::plugins::PluginArtifactMetadata,
    ) -> Result<bool, AdminError> {
        if instance.artifact_sha256 != metadata.sha256 {
            return Err(AdminError::invalid("插件配置与制品不匹配"));
        }
        // 安装时已验证声明；只读列表不重新解包，CPU 工作仍受统一校验容量约束
        let slot = self
            .validators
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AdminError::unavailable("插件校验已停止"))?;
        let schema = metadata.configuration_schema.clone();
        let secret_fields = metadata.secret_fields.iter().cloned().collect();
        let schemas = Arc::clone(&self.configuration_schemas);
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            super::super::configuration::configuration_ready(
                &instance,
                &schema,
                &secret_fields,
                &schemas,
            )
        })
        .await
        .map_err(|_| AdminError::internal("插件校验任务失败"))?
    }

    async fn validate(
        &self,
        instance: PluginInstance,
    ) -> Result<PluginStateConfiguration, AdminError> {
        let slot = self
            .validators
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AdminError::unavailable("插件校验已停止"))?;
        let artifact = self
            .store
            .load_artifact(&instance.artifact_sha256)
            .await
            .map_err(|_| AdminError::not_found("插件制品不存在"))?;
        let limits = self.config.package_limits;
        let schemas = Arc::clone(&self.configuration_schemas);
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let package =
                ValidatedPackage::read(artifact.archive, Some(&instance.artifact_sha256), limits)
                    .map_err(|_| AdminError::invalid("插件制品校验失败"))?;
            let (_, state) =
                super::super::configuration::validate(&instance, package.manifest(), &schemas)?;
            crate::adapter::observer::validate_bindings(package.manifest(), &instance.bindings)?;
            crate::adapter::policy::validate_bindings(package.manifest(), &instance.bindings)?;
            crate::adapter::middleware::validate_bindings(package.manifest(), &instance.bindings)?;
            crate::adapter::catalog::validate_bindings(package.manifest(), &instance.bindings)?;
            crate::adapter::upstream_adapter::validate_bindings(
                package.manifest(),
                &instance.bindings,
            )?;
            crate::adapter::frontend_authentication::validate_bindings(
                package.manifest(),
                &instance.bindings,
            )?;
            Ok(state)
        })
        .await
        .map_err(|_| AdminError::internal("插件校验任务失败"))?
    }
    async fn prepare(
        &self,
        snapshot: PluginInstanceSnapshot,
    ) -> Result<ExtensionSetReference, AdminError> {
        let required_revision = snapshot.config_revision;
        self.prepare_snapshot(snapshot, Some(required_revision), None)
            .await
    }
}

impl ExtensionPreparationPort for PluginRuntime {
    fn prepare_with_request_body_limit(
        &self,
        revision: ConfigRevision,
        request_body_limit: usize,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>> {
        self.prepare_revision(revision, Some(request_body_limit))
    }

    fn prepare(
        &self,
        revision: ConfigRevision,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>> {
        self.prepare_revision(revision, None)
    }
}

impl PluginRuntime {
    fn prepare_revision(
        &self,
        revision: ConfigRevision,
        request_body_limit: Option<usize>,
    ) -> BoxFuture<'_, Result<ExtensionSetReference, ExtensionPreparationError>> {
        Box::pin(async move {
            let snapshot = self
                .store
                .load_instances()
                .await
                .map_err(|_| ExtensionPreparationError)?;
            if snapshot.config_revision.get() != revision.get() {
                return Err(ExtensionPreparationError);
            }
            self.prepare_snapshot(snapshot, None, request_body_limit)
                .await
                .map_err(|_| ExtensionPreparationError)
        })
    }
}
