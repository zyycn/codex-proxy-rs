//! 上游额度展示快照的批量读取、刷新抢占和连接版本隔离写入

use super::*;
use gateway_core::account::{
    UpstreamQuotaRefreshClaim, UpstreamQuotaRefreshResult, UpstreamQuotaSnapshot,
};

fn failure(source: sqlx::Error) -> CoreStoreError {
    CoreStoreError::caused_by(CoreStoreErrorKind::Unavailable, source)
}

fn revision(value: u64) -> Result<i64, CoreStoreError> {
    i64::try_from(value).map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))
}

pub(super) async fn read(
    pool: &PgPool,
    accounts: &[CoreProviderAccountId],
) -> Result<Vec<UpstreamQuotaSnapshot>, CoreStoreError> {
    let ids = accounts.iter().map(|id| id.as_str()).collect::<Vec<_>>();
    let rows = sqlx::query(
        "select p.id, p.credential_revision, s.fetched_at, s.last_attempt_at,
                s.last_error_code, s.forwarding_blocked, s.payload
         from provider_accounts p
         left join provider_account_upstream_quota_snapshots s
           on s.account_id=p.id and s.credential_revision=p.credential_revision
         where p.id=any($1) and p.enabled and p.provider_kind='openai'
           and p.authentication_kind='api_key'
           and p.provider_credentials_json->>'fetch_upstream_quota'='true'",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .map_err(failure)?;
    rows.into_iter()
        .map(|row| {
            let payload: Option<sqlx::types::Json<serde_json::Map<String, serde_json::Value>>> =
                row.try_get("payload").map_err(failure)?;
            Ok(UpstreamQuotaSnapshot {
                account_id: CoreProviderAccountId::new(
                    row.try_get::<String, _>("id").map_err(failure)?,
                )
                .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                credential_revision: CoreCredentialRevision::new(
                    u64::try_from(
                        row.try_get::<i64, _>("credential_revision")
                            .map_err(failure)?,
                    )
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                )
                .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                fetched_at: row
                    .try_get::<Option<DateTime<Utc>>, _>("fetched_at")
                    .map_err(failure)?
                    .map(Into::into),
                last_attempt_at: row
                    .try_get::<Option<DateTime<Utc>>, _>("last_attempt_at")
                    .map_err(failure)?
                    .map(Into::into),
                last_error_code: row.try_get("last_error_code").map_err(failure)?,
                forwarding_blocked: row
                    .try_get::<Option<bool>, _>("forwarding_blocked")
                    .map_err(failure)?
                    .unwrap_or(false),
                payload: payload.map(|value| OpaqueProviderData::new(value.0)),
            })
        })
        .collect()
}

pub(super) async fn claim(
    pool: &PgPool,
    claim: &UpstreamQuotaRefreshClaim,
) -> Result<bool, CoreStoreError> {
    let attempted_at = DateTime::<Utc>::from(claim.attempted_at);
    let interval = chrono::TimeDelta::from_std(claim.minimum_interval)
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    let result = sqlx::query(
        "insert into provider_account_upstream_quota_snapshots
           (account_id, credential_revision, attempt_id, last_attempt_at)
         select p.id, p.credential_revision, $3, $4 from provider_accounts p
         where p.id=$1 and p.credential_revision=$2 and p.enabled
           and p.provider_kind='openai' and p.authentication_kind='api_key'
           and p.provider_credentials_json->>'fetch_upstream_quota'='true'
         on conflict(account_id) do update set
           credential_revision=excluded.credential_revision, attempt_id=excluded.attempt_id,
           last_attempt_at=excluded.last_attempt_at,
           payload=case when provider_account_upstream_quota_snapshots.credential_revision=excluded.credential_revision
             then provider_account_upstream_quota_snapshots.payload end,
           fetched_at=case when provider_account_upstream_quota_snapshots.credential_revision=excluded.credential_revision
             then provider_account_upstream_quota_snapshots.fetched_at end,
           forwarding_blocked=case when provider_account_upstream_quota_snapshots.credential_revision=excluded.credential_revision
             then provider_account_upstream_quota_snapshots.forwarding_blocked else false end,
           last_error_code=case when provider_account_upstream_quota_snapshots.credential_revision=excluded.credential_revision
             then provider_account_upstream_quota_snapshots.last_error_code end
         where provider_account_upstream_quota_snapshots.credential_revision<>excluded.credential_revision
            or provider_account_upstream_quota_snapshots.last_attempt_at is null
            or provider_account_upstream_quota_snapshots.last_attempt_at<=$5",
    ).bind(claim.account_id.as_str()).bind(revision(claim.credential_revision.get())?)
        .bind(&claim.attempt_id).bind(attempted_at).bind(attempted_at-interval)
        .execute(pool).await.map_err(failure)?;
    Ok(result.rows_affected() == 1)
}

pub(super) async fn finish(
    pool: &PgPool,
    result: UpstreamQuotaRefreshResult,
) -> Result<bool, CoreStoreError> {
    let payload = result
        .payload
        .map(|data| sqlx::types::Json(data.into_inner()));
    let updated = sqlx::query(
        "update provider_account_upstream_quota_snapshots s set
           payload=case when $4::text is null then $5::jsonb else s.payload end,
           fetched_at=case when $4::text is null then now() else s.fetched_at end,
           forwarding_blocked=case
             when $4::text is null then false
             when $4::text in ('unauthorized', 'forbidden', 'sharing_disabled') then true
             else s.forwarding_blocked end,
           last_error_code=$4, attempt_id=null
         from provider_accounts p
         where s.account_id=$1 and s.credential_revision=$2 and s.attempt_id=$3
           and p.id=s.account_id and p.credential_revision=$2 and p.enabled
           and p.provider_credentials_json->>'fetch_upstream_quota'='true'",
    )
    .bind(result.claim.account_id.as_str())
    .bind(revision(result.claim.credential_revision.get())?)
    .bind(result.claim.attempt_id)
    .bind(result.error_code)
    .bind(payload)
    .execute(pool)
    .await
    .map_err(failure)?;
    Ok(updated.rows_affected() == 1)
}
