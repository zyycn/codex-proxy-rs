//! 基于认证后账号范围的只读额度导出和管理端子额度查询

use std::{collections::BTreeMap, sync::Arc};

use gateway_core::{account::ProviderAccountId, policy::ClientPolicy, routing::ProviderKind};
use gateway_protocol::quota::{MAX_QUOTA_RESPONSE_BYTES, QuotaResponse};
use hkdf::Hkdf;
use secrecy::ExposeSecret as _;
use sha2::Sha256;

use crate::{
    model::{
        AdminError,
        upstream_quota::{AccountUpstreamQuota, QuotaExportError},
    },
    ports::{provider::ProviderAdminRegistry, store::ClientKeyStore},
};

pub struct UpstreamQuotaService {
    keys: Arc<dyn ClientKeyStore>,
    providers: ProviderAdminRegistry,
}

impl UpstreamQuotaService {
    pub(crate) fn new(keys: Arc<dyn ClientKeyStore>, providers: ProviderAdminRegistry) -> Self {
        Self { keys, providers }
    }

    pub async fn snapshots(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<BTreeMap<ProviderAccountId, AccountUpstreamQuota>, AdminError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let provider = self
            .providers
            .require(&openai_kind())
            .map_err(|error| super::map_provider_error(error, "upstream quota provider"))?;
        provider
            .upstream_quota_snapshots(ids)
            .await
            .map_err(|error| super::map_provider_error(error, "upstream quota snapshots"))
    }

    pub async fn export(&self, policy: &ClientPolicy) -> Result<QuotaResponse, QuotaExportError> {
        let identity = self
            .keys
            .quota_export_identity(policy.key_id())
            .await
            .map_err(|_| QuotaExportError::Unavailable)?
            .ok_or(QuotaExportError::Unauthorized)?;
        if !identity.enabled {
            return Err(QuotaExportError::Unauthorized);
        }
        if !identity.sharing.enabled {
            return Err(QuotaExportError::SharingDisabled);
        }
        let provider = self
            .providers
            .require(&openai_kind())
            .map_err(|_| QuotaExportError::Unavailable)?;
        let ids = policy
            .account_scope()
            .account_ids()
            .cloned()
            .collect::<Vec<_>>();
        let entries = provider
            .quota_accounts(&ids, identity.sharing.include_models)
            .await
            .map_err(|_| QuotaExportError::Unavailable)?;
        let derivation = Hkdf::<Sha256>::new(
            Some(policy.key_id().as_str().as_bytes()),
            identity.secret.expose_secret().as_bytes(),
        );
        let mut accounts = Vec::with_capacity(entries.len());
        for (source, mut entry) in entries {
            let info = serde_json::to_vec(&(source.as_str(), entry.id.as_str()))
                .map_err(|_| QuotaExportError::Unavailable)?;
            let mut public_id = [0_u8; 32];
            derivation
                .expand(&info, &mut public_id)
                .map_err(|_| QuotaExportError::Unavailable)?;
            entry.id = hex::encode(public_id);
            // 每一跳独立执行当前 Key 的披露策略，名称仅在管理员明确授权时沿用
            if !identity.sharing.include_names {
                entry.label = format!("上游账号 {}", accounts.len() + 1);
            }
            if !identity.sharing.include_models {
                entry.models = None;
            } else if let Some(models) = &mut entry.models {
                models.retain(|model| policy.account_scope().allows_model(&source, model));
            }
            accounts.push(entry);
        }
        let response = QuotaResponse {
            schema_version: 1,
            accounts,
        };
        response
            .validate()
            .map_err(|_| QuotaExportError::Unavailable)?;
        if serde_json::to_vec(&response)
            .map_err(|_| QuotaExportError::Unavailable)?
            .len()
            > MAX_QUOTA_RESPONSE_BYTES
        {
            return Err(QuotaExportError::ResponseTooLarge);
        }
        Ok(response)
    }
}

fn openai_kind() -> ProviderKind {
    ProviderKind::new("openai").expect("内置 Provider 标识有效")
}
