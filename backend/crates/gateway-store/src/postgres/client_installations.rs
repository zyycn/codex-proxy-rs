//! PostgreSQL 持久化 Client Key 与 OpenAI 账号的 installation id

use futures::future::BoxFuture;
use gateway_core::provider_ports::{
    ProviderClientInstallationKey, ProviderClientInstallationPort, ProviderStoreError,
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgClientInstallationRepository {
    pool: PgPool,
}

impl PgClientInstallationRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl ProviderClientInstallationPort for PgClientInstallationRepository {
    fn read<'a>(
        &'a self,
        key: &'a ProviderClientInstallationKey,
    ) -> BoxFuture<'a, Result<Option<Uuid>, ProviderStoreError>> {
        Box::pin(async move {
            sqlx::query_scalar(
                "select installation_id from openai_client_installations
                 where client_api_key_id = $1 and provider_account_id = $2",
            )
            .bind(key.client_api_key_id().as_str())
            .bind(key.provider_account_id().as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| crate::provider_unavailable("read client installation", source))
        })
    }

    fn get_or_insert<'a>(
        &'a self,
        key: &'a ProviderClientInstallationKey,
        candidate: Uuid,
    ) -> BoxFuture<'a, Result<Uuid, ProviderStoreError>> {
        Box::pin(async move {
            let inserted = sqlx::query_scalar(
                "insert into openai_client_installations
                    (client_api_key_id, provider_account_id, installation_id)
                 values ($1, $2, $3)
                 on conflict (client_api_key_id, provider_account_id) do nothing
                 returning installation_id",
            )
            .bind(key.client_api_key_id().as_str())
            .bind(key.provider_account_id().as_str())
            .bind(candidate)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| crate::provider_unavailable("insert client installation", source))?;
            if let Some(installation_id) = inserted {
                return Ok(installation_id);
            }

            // 冲突可能来自语句快照之后才提交的行，独立查询才能读取胜出的 UUID
            self.read(key).await?.ok_or_else(|| {
                crate::provider_unavailable(
                    "read committed client installation",
                    sqlx::Error::RowNotFound,
                )
            })
        })
    }
}
