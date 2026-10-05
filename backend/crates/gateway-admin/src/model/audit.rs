//! 管理写入的审计分类；适配器只能补充事务实际写入的字段与实体标识

use super::{
    MutationActor, MutationContext,
    auth::{AdminAuditEvent, AuditActorKind, admin_session_actor_ref},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationAuditOperation {
    ModelPricingSync,
    ModelPricingUpdate,
    RuntimeSettingsReplace,
    AdminApiKeyChanged { exists: bool },
    AccountGroupCreate,
    AccountGroupUpdate,
    AccountGroupEnabled { enabled: bool },
    AccountGroupDelete,
    ClientApiKeyResetBudget,
    ProviderAccountUpdate,
    BackupStorageUpdate,
    BackupScheduleUpdate,
    ProviderAccountAdaptConcurrency,
    ProviderAccountRecover,
    ProviderAccountBatchUpdate,
    ProviderAccountDelete,
    ProviderAccountExportCredentials,
    PluginArtifactInstall,
    PluginArtifactAccept,
    PluginArtifactDelete,
    PluginInstanceConfigure,
    PluginInstanceDelete,
    PluginSourceCredentialCreate,
    PluginSourceCredentialDelete,
    ClientApiKeyUpdateBudgetLimits,
    ClientApiKeyCreate,
    ClientApiKeyUpdate,
    ClientApiKeyEnabled { enabled: bool },
    ClientApiKeyDelete,
    PluginSourceChangeSource,
    PluginSourceDelete,
    OutboundProxyCreate,
    OutboundProxyUpdate,
    OutboundProxyDelete,
    OutboundProxyTest,
    AccountGroupPluginReconcile,
    ClientApiKeyPluginReconcile,
    ProviderAccountImportDocument,
    ProviderAccountRotateCredential,
    ProviderAccountRefreshCredential,
    ProviderAccountAuthorize,
    ProviderAccountReauthorize,
}

impl MutationAuditOperation {
    #[must_use]
    pub const fn classification(self) -> (&'static str, &'static str) {
        match self {
            Self::ModelPricingSync => ("pricing.sync", "model_pricing"),
            Self::ModelPricingUpdate => ("pricing.update", "model_pricing"),
            Self::RuntimeSettingsReplace => ("settings.replace", "runtime_settings"),
            Self::AdminApiKeyChanged { exists } => (
                if exists {
                    "admin_api_key.replace"
                } else {
                    "admin_api_key.delete"
                },
                "runtime_settings",
            ),
            Self::AccountGroupCreate => ("create", "account_group"),
            Self::AccountGroupUpdate => ("update", "account_group"),
            Self::AccountGroupEnabled { enabled } => {
                (if enabled { "enable" } else { "disable" }, "account_group")
            }
            Self::AccountGroupDelete => ("delete", "account_group"),
            Self::ClientApiKeyResetBudget => ("reset_budget", "client_api_key"),
            Self::ProviderAccountUpdate => ("update", "provider_account"),
            Self::BackupStorageUpdate => ("backup.s3_config_updated", "backup_settings"),
            Self::BackupScheduleUpdate => ("backup.schedule_updated", "backup_settings"),
            Self::ProviderAccountAdaptConcurrency => ("adapt_concurrency", "provider_account"),
            Self::ProviderAccountRecover => ("recover", "provider_account"),
            Self::ProviderAccountBatchUpdate => ("batch_update", "provider_account"),
            Self::ProviderAccountDelete => ("delete", "provider_account"),
            Self::ProviderAccountExportCredentials => ("export_credentials", "provider_account"),
            Self::PluginArtifactInstall => ("install", "plugin_artifact"),
            Self::PluginArtifactAccept => ("accept", "plugin_artifact"),
            Self::PluginArtifactDelete => ("delete", "plugin_artifact"),
            Self::PluginInstanceConfigure => ("configure", "plugin_instance"),
            Self::PluginInstanceDelete => ("delete", "plugin_instance"),
            Self::PluginSourceCredentialCreate => ("create", "plugin_source_credential"),
            Self::PluginSourceCredentialDelete => ("delete", "plugin_source_credential"),
            Self::ClientApiKeyUpdateBudgetLimits => ("update_budget_limits", "client_api_key"),
            Self::ClientApiKeyCreate => ("create", "client_api_key"),
            Self::ClientApiKeyUpdate => ("update", "client_api_key"),
            Self::ClientApiKeyEnabled { enabled } => {
                (if enabled { "enable" } else { "disable" }, "client_api_key")
            }
            Self::ClientApiKeyDelete => ("delete", "client_api_key"),
            Self::PluginSourceChangeSource => ("change_source", "plugin_source"),
            Self::PluginSourceDelete => ("delete", "plugin_source"),
            Self::OutboundProxyCreate => ("create", "outbound_proxy"),
            Self::OutboundProxyUpdate => ("update", "outbound_proxy"),
            Self::OutboundProxyDelete => ("delete", "outbound_proxy"),
            Self::OutboundProxyTest => ("test", "outbound_proxy"),
            Self::AccountGroupPluginReconcile => ("plugin_reconcile", "account_group"),
            Self::ClientApiKeyPluginReconcile => ("plugin_reconcile", "client_api_key"),
            Self::ProviderAccountImportDocument => ("import_document", "provider_account"),
            Self::ProviderAccountRotateCredential => ("rotate_credential", "provider_account"),
            Self::ProviderAccountRefreshCredential => ("refresh_credential", "provider_account"),
            Self::ProviderAccountAuthorize => ("authorize", "provider_account"),
            Self::ProviderAccountReauthorize => ("reauthorize", "provider_account"),
        }
    }
}

/// 分类与稳定实体标识共同组成审计意图，字段差异由同一提交事务补齐
#[derive(Debug, Clone, Copy)]
pub struct MutationAuditIntent<'a> {
    pub operation: MutationAuditOperation,
    pub entity_ref: &'a str,
}

impl MutationAuditIntent<'_> {
    #[must_use]
    pub fn event(self, context: &MutationContext, changed_fields: Vec<String>) -> AdminAuditEvent {
        let (actor_kind, actor_admin_user_id, actor_ref) = match &context.actor {
            MutationActor::AdminSession { admin_user_id } => (
                AuditActorKind::AdminSession,
                Some(admin_user_id.clone()),
                admin_session_actor_ref(admin_user_id),
            ),
            MutationActor::AdminApiKey => (
                AuditActorKind::AdminApiKey,
                None,
                "admin_api_key".to_owned(),
            ),
            MutationActor::System => (AuditActorKind::System, None, "system".to_owned()),
        };
        let (action, entity_kind) = self.operation.classification();
        AdminAuditEvent {
            id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
            actor_kind,
            actor_admin_user_id,
            actor_ref,
            request_id: Some(context.request_id.clone()),
            action: action.to_owned(),
            entity_kind: entity_kind.to_owned(),
            entity_ref: self.entity_ref.to_owned(),
            config_revision: None,
            changed_fields,
            occurred_at: chrono::Utc::now(),
        }
    }
}
