//! 预测只读采样：先聚合数值、再读取有界的 Provider 文档，不扫描模型排行维度

use gateway_admin::model::quota_forecast::QuotaForecastHistory;

use super::super::*;

pub(in crate::postgres::provider_accounts) async fn load_history(
    pool: &PgPool,
    budget: &crate::postgres::ObservabilityQueryBudget,
    window: &AccountUsageWindowQuery,
) -> AdminStoreResult<QuotaForecastHistory> {
    validate_admin_account_ids(std::slice::from_ref(&window.account_id))
        .map_err(|error| admin_store_error(ENTITY, error))?;
    let range = ObservabilityRange::new(window.range.start, window.range.end)
        .map_err(|error| admin_store_error(ENTITY, error))?;
    if range.end - range.start > TimeDelta::days(32) {
        return Err(AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "forecast range exceeds supported quota windows",
        ));
    }
    budget
        .run(
            pool,
            "load quota forecast history",
            |mut connection| async move {
                let mut snapshot =
                    crate::postgres::observability::AnalyticsSnapshot::new_for_accounts(
                        &mut connection,
                    )
                    .await?;
                let history = snapshot.forecast_history(&window.account_id, range).await?;
                snapshot.finish().await?;
                Ok(history)
            },
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))
}
