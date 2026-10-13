//! 十万条以上账号与 Key 用量的完整性、未知值和汇总读取验收

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use gateway_store::postgres::{
    ObservabilityQueryBudget, ObservabilityRange, PgObservabilityRepository,
    PgRequestAnalyticsRepository, ProviderAccountUsageQuery, UsageRecordFilter,
};
use sqlx::Connection;

use super::super::super::{TestDatabase, analytics::seed};

#[tokio::test]
async fn large_account_and_key_usage_keep_exact_totals_and_read_clean_rollups() {
    let Some(database) = TestDatabase::create("large_account_usage").await else {
        return;
    };
    let start = DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    sqlx::query("insert into provider_accounts(id,provider_kind,name,authentication_kind,provider_credentials_json,credential_revision,has_refresh_token,credential_observed_at,created_at,updated_at) values('account_large','openai','large','oauth','{}',1,false,$1,$1,$1)")
        .bind(start).execute(&database.pool).await.unwrap();
    seed(
        &database.pool,
        "large_template",
        start + TimeDelta::minutes(1),
        &[],
    )
    .await;
    sqlx::query("update model_requests set provider_kind='openai',provider_account_ref='account_large',cached_input_tokens=0,reasoning_output_tokens=null where id='large_template'")
        .execute(&database.pool).await.unwrap();
    // 扩充真实源表并经过生产 Worker，确保回归覆盖账号接口而非只验证派生表
    sqlx::query("insert into model_requests select cloned.* from model_requests template cross join generate_series(1,100005) n cross join lateral jsonb_populate_record(null::model_requests,to_jsonb(template)||jsonb_build_object('id','account_large_'||n::text)) cloned where template.id='large_template'")
        .execute(&database.pool).await.unwrap();
    let budget =
        ObservabilityQueryBudget::try_new(4, Duration::from_secs(10), Duration::from_secs(10))
            .unwrap();
    let worker = PgRequestAnalyticsRepository::new(database.pool.clone(), budget.clone());
    let mut ready = false;
    for _ in 0..400 {
        worker.maintain(1000).await.unwrap();
        let status = worker.status().await.unwrap();
        if !status.building && status.pending_requests == 0 && status.pending_buckets == 0 {
            ready = true;
            break;
        }
    }
    assert!(
        ready,
        "large source dataset must finish projection and rollups"
    );
    let repo = PgObservabilityRepository::new(database.pool.clone(), None, budget, None);
    let range = ObservabilityRange::new(start, start + TimeDelta::hours(1)).unwrap();
    let query =
        ProviderAccountUsageQuery::for_accounts(range, vec!["account_large".to_owned()]).unwrap();
    let mut blocker = sqlx::PgConnection::connect_with(&database.pool.connect_options())
        .await
        .unwrap();
    sqlx::raw_sql("begin;lock table request_analytics_facts in access exclusive mode")
        .execute(&mut blocker)
        .await
        .unwrap();
    let account = tokio::time::timeout(Duration::from_secs(2), repo.provider_account_usage(query))
        .await
        .expect("clean account usage must not read facts")
        .unwrap()
        .remove(0);
    assert_eq!(account.request_count, 100006);
    assert_eq!(account.total_tokens, Some(100006 * 120));
    assert_eq!(account.cached_input_tokens, Some(0));
    assert_eq!(account.reasoning_output_tokens, None);
    assert_eq!(account.models.len(), 1);
    assert_eq!(account.models[0].request_count, 100006);
    assert_eq!(account.last_used_at, Some(start + TimeDelta::minutes(1)));
    let filter = UsageRecordFilter {
        client_api_key_ref: Some("key_analytics".to_owned()),
        model: Some("model_a".to_owned()),
        ..Default::default()
    };
    let key = tokio::time::timeout(Duration::from_secs(2), repo.usage_summary(range, filter))
        .await
        .expect("Key plus model must use joint rollups")
        .unwrap();
    assert_eq!(key.requests.request_count, 100006);
    assert_eq!(key.providers.len(), 1);
    assert_eq!(key.providers[0].request_count, 100006);
    sqlx::raw_sql("rollback")
        .execute(&mut blocker)
        .await
        .unwrap();
    drop(blocker);
    let partial = ObservabilityRange::new(
        start + TimeDelta::seconds(30),
        start + TimeDelta::minutes(2),
    )
    .unwrap();
    let account = repo
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(partial, vec!["account_large".to_owned()])
                .unwrap(),
        )
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        account.request_count, 100006,
        "the boundary aggregation must not truncate a hot bucket"
    );
    assert_eq!(account.total_tokens, Some(100006 * 120));
    database.close().await;
}
