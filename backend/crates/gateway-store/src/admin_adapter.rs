//! Admin 认证与设置 adapter。

use super::*;

pub(crate) struct AdminAuthStoreAdapter {
    pub(crate) security: postgres::PgAdminSecurityAuditRepository,
    pub(crate) settings: postgres::PgRuntimeSettingsRepository,
    pub(crate) state: redis::RedisAdminAuthStateRepository,
}

pub(crate) struct AdminSettingsStoreAdapter {
    pub(crate) control_plane: postgres::PgControlPlaneRepository,
}

#[async_trait::async_trait]
impl SettingsStore for AdminSettingsStoreAdapter {
    async fn load_runtime_settings(&self) -> AdminStoreResult<AdminRuntimeSettings> {
        let snapshot = postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map_err(|error| admin_store_error("runtime settings", error))?;
        admin_runtime_settings(snapshot.settings)
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map(|snapshot| snapshot.settings.admin_api_key.is_some())
            .map_err(|error| admin_store_error("admin API key", error))
    }

    async fn replace_runtime_settings(
        &self,
        command: ReplaceRuntimeSettings,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRuntimeSettings> {
        let current = postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map_err(|error| admin_store_error("runtime settings", error))?;
        let replacement = postgres::ControlPlaneReplacement {
            settings: postgres::RuntimeSettingsUpdate {
                admin_api_key: current.settings.admin_api_key,
                refresh_margin_seconds: command.refresh_margin_seconds,
                refresh_concurrency: command.refresh_concurrency,
                max_concurrent_per_account: command.max_concurrent_per_account,
                request_interval_ms: command.request_interval_ms,
                rotation_strategy: command.rotation_strategy.as_str().to_owned(),
                model_mappings: store_model_mappings(command.model_mappings),
                min_codex_desktop_version: command.min_codex_desktop_version,
                min_codex_cli_version: command.min_codex_cli_version,
                usage_retention_days: command.usage_retention_days,
                ops_event_retention_days: command.ops_event_retention_days,
                audit_retention_days: command.audit_retention_days,
            },
            audit: mutation_audit(
                context,
                "settings.replace",
                "runtime_settings",
                "1",
                vec![
                    "model_mappings_json".to_owned(),
                    "refresh_margin_seconds".to_owned(),
                    "refresh_concurrency".to_owned(),
                    "max_concurrent_per_account".to_owned(),
                    "request_interval_ms".to_owned(),
                    "rotation_strategy".to_owned(),
                    "min_codex_desktop_version".to_owned(),
                    "min_codex_cli_version".to_owned(),
                    "retention".to_owned(),
                ],
            ),
        };
        let snapshot = postgres::ControlPlaneRepository::replace_control_plane(
            &self.control_plane,
            replacement,
        )
        .await
        .map_err(|error| admin_store_error("runtime settings", error))?;
        admin_runtime_settings(snapshot.settings)
    }

    async fn replace_admin_api_key(
        &self,
        key: AdminApiKey,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        self.replace_admin_api_key_value(Some(key.expose_for_auth().to_owned()), context)
            .await
    }

    async fn delete_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        self.replace_admin_api_key_value(None, context).await
    }
}

impl AdminSettingsStoreAdapter {
    async fn replace_admin_api_key_value(
        &self,
        admin_api_key: Option<String>,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        let exists = admin_api_key.is_some();
        let revision = postgres::ControlPlaneRepository::replace_admin_api_key(
            &self.control_plane,
            admin_api_key,
            mutation_audit(
                context,
                if exists {
                    "admin_api_key.replace"
                } else {
                    "admin_api_key.delete"
                },
                "runtime_settings",
                "1",
                vec!["admin_api_key".to_owned()],
            ),
        )
        .await
        .map_err(|error| admin_store_error("admin API key", error))?;
        Ok(AdminApiKeyMutation {
            config_revision: admin_revision(revision)?,
            exists,
        })
    }
}

pub(crate) fn admin_runtime_settings(
    settings: postgres::RuntimeSettings,
) -> AdminStoreResult<AdminRuntimeSettings> {
    let rotation_strategy = AdminRotationStrategy::parse(settings.rotation_strategy.as_str())
        .ok_or_else(|| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                "runtime settings",
                "rotation strategy is invalid",
            )
        })?;
    let model_mappings = settings
        .model_mappings
        .into_iter()
        .map(|(public, upstream)| {
            let public = gateway_core::routing::PublicModelId::new(public).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    "runtime settings",
                    "public model mapping is invalid",
                )
            })?;
            let upstream = gateway_core::routing::UpstreamModelId::new(upstream).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    "runtime settings",
                    "upstream model mapping is invalid",
                )
            })?;
            Ok((public, upstream))
        })
        .collect::<AdminStoreResult<ModelMappings>>()?;
    Ok(AdminRuntimeSettings {
        config_revision: admin_revision(settings.config_revision)?,
        model_mappings,
        refresh_margin_seconds: settings.refresh_margin_seconds,
        refresh_concurrency: settings.refresh_concurrency,
        max_concurrent_per_account: settings.max_concurrent_per_account,
        request_interval_ms: settings.request_interval_ms,
        rotation_strategy,
        min_codex_desktop_version: settings.min_codex_desktop_version,
        min_codex_cli_version: settings.min_codex_cli_version,
        usage_retention_days: settings.usage_retention_days,
        ops_event_retention_days: settings.ops_event_retention_days,
        audit_retention_days: settings.audit_retention_days,
        updated_at: settings.updated_at,
    })
}

pub(crate) fn store_model_mappings(
    mappings: ModelMappings,
) -> std::collections::BTreeMap<String, String> {
    mappings
        .into_iter()
        .map(|(public, upstream)| (public.as_str().to_owned(), upstream.as_str().to_owned()))
        .collect()
}

#[async_trait::async_trait]
impl AuthStore for AdminAuthStoreAdapter {
    async fn load_password_hash(&self, admin_user_id: &str) -> AdminStoreResult<Option<String>> {
        postgres::AdminSecurityAuditRepository::password_hash(&self.security, admin_user_id)
            .await
            .map_err(|error| admin_store_error("admin authentication", error))
    }

    async fn create_password_hash_if_absent(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> AdminStoreResult<bool> {
        postgres::AdminSecurityAuditRepository::create_password_hash_if_absent(
            &self.security,
            admin_user_id,
            password_hash,
        )
        .await
        .map_err(|error| admin_store_error("admin authentication", error))
    }

    async fn load_admin_api_key(&self) -> AdminStoreResult<Option<AdminApiKey>> {
        postgres::RuntimeSettingsRepository::load_runtime_settings(&self.settings)
            .await
            .map(|settings| settings.admin_api_key.map(AdminApiKey::new))
            .map_err(|error| admin_store_error("admin API key", error))
    }

    async fn load_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        redis::AdminAuthStateRepository::load_admin_session(&self.state, session_id)
            .await
            .map(|session| {
                session.map(|record| AdminSession {
                    admin_user_id: record.admin_user_id,
                    expires_at: record.expires_at,
                })
            })
            .map_err(|error| admin_store_error("admin session", error))
    }

    async fn store_session(
        &self,
        session_id: &str,
        session: &AdminSession,
    ) -> AdminStoreResult<()> {
        redis::AdminAuthStateRepository::store_admin_session(
            &self.state,
            session_id,
            &redis::AdminSessionRecord {
                admin_user_id: session.admin_user_id.clone(),
                expires_at: session.expires_at,
            },
        )
        .await
        .map_err(|error| admin_store_error("admin session", error))
    }

    async fn delete_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        redis::AdminAuthStateRepository::delete_admin_session(&self.state, session_id)
            .await
            .map(|session| {
                session.map(|record| AdminSession {
                    admin_user_id: record.admin_user_id,
                    expires_at: record.expires_at,
                })
            })
            .map_err(|error| admin_store_error("admin session", error))
    }

    async fn append_audit_event(&self, event: AdminAuditModel) -> AdminStoreResult<()> {
        let config_revision = event
            .config_revision
            .map(|revision| i64::try_from(revision.get()))
            .transpose()
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    "admin audit",
                    "config revision is outside the supported range",
                )
            })?;
        let actor_kind = match event.actor_kind {
            gateway_admin::model::auth::AuditActorKind::AdminSession => {
                postgres::AdminAuditActorKind::AdminSession
            }
            gateway_admin::model::auth::AuditActorKind::AdminApiKey => {
                postgres::AdminAuditActorKind::AdminApiKey
            }
            gateway_admin::model::auth::AuditActorKind::System => {
                postgres::AdminAuditActorKind::System
            }
            gateway_admin::model::auth::AuditActorKind::Anonymous => {
                postgres::AdminAuditActorKind::Anonymous
            }
        };
        postgres::AdminSecurityAuditRepository::append_admin_audit_event(
            &self.security,
            postgres::AdminAuditEvent {
                id: event.id,
                actor_kind,
                actor_admin_user_id: event.actor_admin_user_id,
                actor_ref: event.actor_ref,
                admin_request_id: event.request_id,
                action: event.action,
                entity_kind: event.entity_kind,
                entity_ref: event.entity_ref,
                config_revision,
                changed_fields: event.changed_fields,
                created_at: event.occurred_at,
            },
        )
        .await
        .map_err(|error| admin_store_error("admin audit", error))
    }
}
