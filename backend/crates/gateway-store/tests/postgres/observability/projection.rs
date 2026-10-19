//! 验证观测视图普通聚合能够使用覆盖索引

use chrono::Utc;
use serde_json::Value;

use super::{TestDatabase, seed_observability_facts};

#[tokio::test]
async fn plain_observation_aggregates_can_use_a_covering_index() {
    let Some(db) = TestDatabase::create("observation_covering_index").await else {
        return;
    };
    seed_observability_facts(&db.pool, Utc::now())
        .await
        .unwrap();
    let mut transaction = db.pool.begin().await.unwrap();
    // 视图通过请求 ID 关联运行态租约，覆盖索引包含关联键但不包含观测 JSON
    // 小夹具只验证覆盖索引路径可达，不把耗时阈值或生产规划器成本固定在测试中
    sqlx::raw_sql(
        "create index observation_plain_cover_idx on model_requests (started_at)
           include (id, input_tokens, cost_amount) where recovered_at is null;
         set local enable_seqscan = off",
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    let plan: Value = sqlx::query_scalar(
        "explain (format json)
         select count(*), sum(input_tokens), sum(cost_amount)
           from model_request_observations
          where recovered_at is null and started_at >= now() - interval '1 day'",
    )
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert!(
        uses_covering_index(&plan[0]["Plan"]),
        "plain aggregates must not require fetching observation JSON: {plan}"
    );
    transaction.rollback().await.unwrap();
    db.close().await;
}

fn uses_covering_index(plan: &Value) -> bool {
    (plan["Node Type"] == "Index Only Scan" && plan["Index Name"] == "observation_plain_cover_idx")
        || plan["Plans"]
            .as_array()
            .is_some_and(|children| children.iter().any(uses_covering_index))
}
