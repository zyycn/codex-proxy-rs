use super::{
    AdminError, Arc, Duration, Handshake, PluginCallbackPorts, PluginCallbacks, PluginInstance,
    PluginPrivateState, PluginRuntime, PluginStateStoreErrorKind, PreparedContributions,
    PreparedInstance, Registration, RestartIdentity, Revision, RpcSession, Stage, ValidatedPackage,
    instance_fingerprint,
};

impl PluginRuntime {
    pub(super) async fn prepare_instance(
        &self,
        instance: PluginInstance,
        target_revision: Revision,
    ) -> Result<PreparedContributions, AdminError> {
        let mut sessions = Vec::new();
        let mut observer_entries = Vec::new();
        let mut commands = Vec::new();
        let mut management = Vec::new();
        let mut policy_entries = Vec::new();
        let mut authentication_entries = Vec::new();
        let authorization_binding = instance_fingerprint(&instance)?;
        if self.restart_circuit_is_open(&RestartIdentity::new(
            instance.id.clone(),
            authorization_binding.clone(),
        )) {
            return Err(AdminError::unavailable(
                "插件实例连续异常退出，已暂停自动重启；请检查配置后重新启用",
            ));
        }
        let artifact = self
            .store
            .load_artifact(&instance.artifact_sha256)
            .await
            .map_err(|_| AdminError::unavailable("插件制品不可用"))?;
        let limits = self.config.package_limits;
        let directory = self.config.cache_directory.clone();
        let host_version = self.config.host_version.clone();
        let validation_slot = self
            .validators
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdminError::unavailable("插件校验繁忙"))?;
        let (package, configuration, instance, granted_permissions, state_configuration) =
            tokio::task::spawn_blocking(move || {
                let _validation_slot = validation_slot;
                let package = Arc::new(
                    ValidatedPackage::read(
                        artifact.archive,
                        Some(&instance.artifact_sha256),
                        limits,
                    )
                    .map_err(|_| AdminError::invalid("插件恢复包校验失败"))?,
                );
                let (configuration, permissions, state_configuration) =
                    super::super::configuration::validate(&instance, package.manifest())?;
                crate::adapter::observer::validate_bindings(
                    package.manifest(),
                    &instance.bindings,
                )?;
                crate::adapter::policy::validate_bindings(package.manifest(), &instance.bindings)?;
                crate::adapter::frontend_authentication::validate_bindings(
                    package.manifest(),
                    &instance.bindings,
                )?;
                // 发行清单与当前 Runtime 共用同一份能力声明，不能让二者分别漂移。
                if !crate::package::host_supports(package.manifest())? {
                    return Err(AdminError::invalid("该插件声明的业务能力尚未接入运行时"));
                }
                if !package
                    .manifest()
                    .engines
                    .codex_proxy_rs
                    .matches(&host_version)
                {
                    return Err(AdminError::invalid(format!(
                        "插件要求宿主 {}，当前为 {}",
                        package.manifest().engines.codex_proxy_rs,
                        host_version
                    )));
                }
                let package = Arc::new(
                    package
                        .prepare(&directory, &host_version)
                        .map_err(|_| AdminError::unavailable("插件制品准备失败"))?,
                );
                Ok((
                    package,
                    configuration,
                    instance,
                    permissions,
                    state_configuration,
                ))
            })
            .await
            .map_err(|_| AdminError::internal("插件校验任务失败"))??;
        let manifest = package.package().manifest().clone();
        let plugin_id = manifest
            .plugin_id()
            .map_err(|_| AdminError::invalid("插件身份无效"))?;
        let instance_id = instance.id.clone();
        let restart_identity =
            RestartIdentity::new(instance_id.clone(), authorization_binding.clone());
        let bindings = instance.bindings.clone();
        let owner = self
            .state
            .load_owner(
                gateway_admin::model::plugins::state::PluginStateOwnerRequest {
                    instance_id: instance.id.clone(),
                    artifact_sha256: instance.artifact_sha256.clone(),
                    instance_revision: instance.revision,
                    configuration: state_configuration.clone(),
                },
            )
            .await
            .map_err(|error| match error.kind() {
                PluginStateStoreErrorKind::Unavailable => {
                    AdminError::unavailable("插件状态存储暂不可用")
                }
                _ => AdminError::invalid("插件状态 owner 无法恢复"),
            })?;
        let private_state = Arc::new(PluginPrivateState::new(
            self.state.clone(),
            &manifest,
            state_configuration,
            owner,
        )?);
        let incarnation = uuid::Uuid::new_v4().to_string();
        let lifecycle = self
            .restart_circuits
            .begin(restart_identity)
            .ok_or_else(|| {
                AdminError::unavailable(
                    "插件实例连续异常退出，已暂停自动重启；请检查配置后重新启用",
                )
            })?;
        let handshake = Handshake {
            protocol_version: gateway_plugin_sdk::PROTOCOL_VERSION,
            artifact_sha256: package.package().digest().into(),
            plugin_id: plugin_id.clone(),
            instance_id: instance_id.clone(),
            generation: target_revision.get(),
            incarnation,
            configuration,
            permissions: granted_permissions.clone(),
            contributes: manifest.contributes.clone(),
        };
        let callbacks = Arc::new(PluginCallbacks::new(
            &instance,
            self.config.rpc_limits.maximum_buffered_body_bytes,
            &manifest,
            self.log_slots.clone(),
            private_state.clone(),
            PluginCallbackPorts::new(
                self.http.clone(),
                self.network_policy.clone(),
                self.account_ports.clone(),
                self.client_key_ports.clone(),
                self.model_ports.clone(),
                self.affinity_ports.clone(),
                self.resource_ports.clone(),
            ),
        )?);
        let session = Arc::new(
            RpcSession::start_supervised(
                package.clone(),
                handshake,
                self.config.rpc_limits,
                &self.processes,
                callbacks.clone(),
                lifecycle,
            )
            .await
            .map_err(|_| AdminError::unavailable("插件进程握手失败"))?,
        );
        let registration = session
            .call(
                "plugin.register",
                session.context(Stage::Registration, Duration::from_secs(5)),
                serde_json::json!({}),
                vec![],
            )
            .await
            .map_err(|_| AdminError::invalid("插件注册失败"))?;
        let descriptor: Registration = serde_json::from_value(registration.result)
            .map_err(|_| AdminError::invalid("插件注册结果无效"))?;
        if descriptor.contributes != manifest.contributes || !registration.payload.is_empty() {
            return Err(AdminError::invalid("插件注册结果与清单不符"));
        }
        commands.extend(
            crate::adapter::command_line::prepare(
                &instance,
                &manifest,
                session.clone(),
                callbacks.clone(),
            )
            .await?,
        );
        if let Some(entry) = crate::adapter::management::prepare(
            &instance,
            package.package(),
            session.clone(),
            callbacks.clone(),
        )
        .await?
        {
            management.push(entry);
        }
        if let Some(entry) = crate::adapter::frontend_authentication::prepare_entry(
            &manifest,
            &bindings,
            session.clone(),
            callbacks.clone(),
        )
        .await?
        {
            authentication_entries.push(entry);
        }
        if let Some(entry) = crate::adapter::observer::compile_entry(
            &manifest,
            &plugin_id,
            &instance_id,
            &bindings,
            &granted_permissions,
            session.clone(),
            callbacks.clone(),
        )? {
            observer_entries.push(entry);
        }
        policy_entries.extend(crate::adapter::policy::compile_entries(
            &manifest,
            &instance_id,
            &bindings,
            &granted_permissions,
            session.clone(),
            callbacks.clone(),
        )?);
        let model_aliases =
            crate::adapter::catalog::prepare(&manifest, &instance, &session).await?;
        sessions.push(PreparedInstance {
            instance_id,
            artifact_sha256: instance.artifact_sha256,
            revision: instance.revision,
            session,
            private_state,
            maintenance: manifest
                .contributes
                .contains_key(&gateway_plugin_sdk::Capability::Maintenance),
        });
        Ok(PreparedContributions {
            sessions,
            observer_entries,
            commands,
            management,
            policy_entries,
            authentication_entries,
            model_aliases,
        })
    }
}
