//! 按账号完成时间索引分页读取历史观测；过滤窗口的协议字段由 Provider 解释

use futures::TryStreamExt as _;
use gateway_admin::model::quota_history::{QuotaDocumentPage, QuotaHistoryCursor};

use super::*;

pub(super) async fn documents(
    pool: &PgPool,
    budget: &crate::postgres::ObservabilityQueryBudget,
    account: &CoreProviderAccountId,
    provider: &gateway_core::routing::ProviderKind,
    created_at: DateTime<Utc>,
    range: TimeRange,
    before: Option<&QuotaHistoryCursor>,
) -> AdminStoreResult<QuotaDocumentPage> {
    const PAGE_SIZE: usize = 64;
    const PAGE_BYTES: usize = 2 * 1024 * 1024;

    let (until, last_id) = before.map_or((range.end, ""), |c| (c.completed_at, c.id.as_str()));
    budget
        .run(pool, "read quota documents", |mut connection| async move {
            let mut rows = sqlx::query_as::<_, (DateTime<Utc>, String, String)>(
                "select completed_at,id,provider_observation_json::text from model_requests
                 where provider_account_ref=$1 and provider_kind=$2 and started_at >= $3
                   and completed_at >= $4 and completed_at < $5 and (completed_at,id) < ($6,$7)
                   and outcome <> 'running' and provider_observation_json is not null
                 order by completed_at desc,id desc limit $8",
            )
            .bind(account.as_str())
            .bind(provider.as_str())
            .bind(created_at)
            .bind(range.start)
            .bind(range.end)
            .bind(until)
            .bind(last_id)
            .bind((PAGE_SIZE + 1) as i64)
            .fetch(&mut *connection);
            let mut documents = Vec::new();
            let mut bytes = 0;
            let mut last = None;
            while let Some((completed_at, id, json)) = rows
                .try_next()
                .await
                .map_err(|error| postgres_unavailable("read quota documents", error))?
            {
                // 首份文档独占一页也要前进，否则大文档会卡住所有后续页
                if documents.len() == PAGE_SIZE
                    || (!documents.is_empty() && bytes + json.len() > PAGE_BYTES)
                {
                    return Ok(QuotaDocumentPage {
                        documents,
                        next_cursor: last,
                    });
                }
                bytes += json.len();
                let document = serde_json::from_str(&json)
                    .map_err(|_| invalid("invalid provider observation"))?;
                documents.push((
                    completed_at,
                    ProviderDocument::new(OpaqueProviderData::new(document)),
                ));
                last = Some(QuotaHistoryCursor { completed_at, id });
            }
            Ok(QuotaDocumentPage {
                documents,
                next_cursor: None,
            })
        })
        .await
        .map_err(|error| admin_store_error(ENTITY, error))
}
