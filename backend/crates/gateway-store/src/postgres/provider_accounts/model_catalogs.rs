//! 账号目录的版本约束写入和按相同正文合并的批量恢复

use super::{
    CoreCredentialRevision, CoreProviderAccountId, CoreStoreError, CoreStoreErrorKind, DateTime,
    PgProviderAccountRepository, ProviderKind, Utc,
};
use gateway_core::account::{AccountCatalogObservation, StoredAccountCatalog};

impl PgProviderAccountRepository {
    pub(super) async fn load_model_catalogs(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<StoredAccountCatalog>, CoreStoreError> {
        // 先聚合轻量归属，再按主键读取代表正文，避免一万个相同账号展开一万份 TOAST
        let rows = sqlx::query_as::<_, (Vec<String>, Vec<i64>, Vec<Option<String>>, Vec<Option<String>>, Vec<DateTime<Utc>>, Vec<u8>)>(
            "select g.ids, g.revisions, g.plans, g.etags, g.times, d.document from (
               select min(c.account_id) representative, array_agg(c.account_id order by c.account_id) ids,
                 array_agg(c.credential_revision order by c.account_id) revisions,
                 array_agg(c.plan_type order by c.account_id) plans, array_agg(c.etag order by c.account_id) etags, array_agg(c.observed_at order by c.account_id) times
               from provider_account_model_catalogs c join provider_accounts a on a.id=c.account_id
               where a.provider_kind=$1 and a.enabled and a.credential_state in ('unknown','ready')
                 and (a.access_token_expires_at is null or a.access_token_expires_at > now())
                 and a.credential_revision=c.credential_revision and a.plan_type is not distinct from c.plan_type
               group by c.document_hash
             ) g join provider_account_model_catalogs d on d.account_id=g.representative")
            .bind(provider.as_str()).fetch_all(&self.pool).await.map_err(unavailable)?;
        rows.into_iter()
            .map(|(ids, revisions, plans, etags, times, document)| {
                let accounts = ids
                    .into_iter()
                    .zip(revisions)
                    .zip(plans)
                    .zip(etags)
                    .zip(times)
                    .map(|((((id, revision), plan), etag), time)| {
                        Ok(AccountCatalogObservation {
                            account_id: CoreProviderAccountId::new(id).map_err(|e| {
                                CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, e)
                            })?,
                            revision: CoreCredentialRevision::new(
                                u64::try_from(revision).map_err(|_| {
                                    CoreStoreError::new(CoreStoreErrorKind::InvalidData)
                                })?,
                            )
                            .map_err(|e| {
                                CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, e)
                            })?,
                            plan,
                            etag,
                            observed_at: time.into(),
                        })
                    })
                    .collect::<Result<_, CoreStoreError>>()?;
                Ok(StoredAccountCatalog {
                    accounts,
                    document: document.into(),
                })
            })
            .collect()
    }

    pub(super) async fn save_model_catalog(
        &self,
        observation: &AccountCatalogObservation,
        document: &[u8],
    ) -> Result<bool, CoreStoreError> {
        let result = sqlx::query("insert into provider_account_model_catalogs (account_id, credential_revision, plan_type, observed_at, document, etag)
            select a.id, a.credential_revision, a.plan_type, $4, $5, $6 from provider_accounts a
            where a.id=$1 and a.credential_revision=$2 and a.plan_type is not distinct from $3 for share
            on conflict (account_id) do update set credential_revision=excluded.credential_revision, plan_type=excluded.plan_type,
            etag=excluded.etag, observed_at=excluded.observed_at, document=excluded.document
            where provider_account_model_catalogs.credential_revision < excluded.credential_revision or
            (provider_account_model_catalogs.credential_revision=excluded.credential_revision and provider_account_model_catalogs.observed_at <= excluded.observed_at)")
            .bind(observation.account_id.as_str()).bind(i64::try_from(observation.revision.get()).map_err(|_|CoreStoreError::new(CoreStoreErrorKind::InvalidData))?)
            .bind(observation.plan.as_deref()).bind(DateTime::<Utc>::from(observation.observed_at)).bind(document).bind(observation.etag.as_deref())
            .execute(&self.pool).await.map_err(unavailable)?;
        Ok(result.rows_affected() == 1)
    }
}

fn unavailable(source: sqlx::Error) -> CoreStoreError {
    CoreStoreError::caused_by(CoreStoreErrorKind::Unavailable, source)
}
