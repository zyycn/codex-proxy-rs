//! 观测视图升级兼容性与普通聚合的覆盖索引规划回归测试

use chrono::Utc;
use serde_json::Value;

use super::{TestDatabase, seed_observability_facts};

#[tokio::test]
async fn projection_upgrade_preserves_observation_values() {
    let Some(db) = TestDatabase::create_through("observation_projection_upgrade", 26).await else {
        return;
    };
    seed_observability_facts(&db.pool, Utc::now())
        .await
        .unwrap();
    sqlx::query(
        "update model_requests
            set request_observation_json = jsonb_set(request_observation_json,
              '{timings,upstream}', '{\"responseMs\":700,\"engineIapiTbtMs\":2.450638}')
          where id = 'req_observe_success'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let before: Vec<Value> =
        sqlx::query_scalar("select to_jsonb(mr) from model_request_observations mr order by id")
            .fetch_all(&db.pool)
            .await
            .unwrap();

    super::super::TEST_MIGRATOR.run(&db.pool).await.unwrap();

    let after: Vec<Value> =
        sqlx::query_scalar("select to_jsonb(mr) from model_request_observations mr order by id")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "projection upgrade must preserve every field"
    );
    let updatable: String = sqlx::query_scalar(
        "select is_updatable from information_schema.views
          where table_schema = current_schema() and table_name = 'model_request_observations'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(updatable, "NO", "observation projection remains read-only");
    db.close().await;
}

#[tokio::test]
async fn plain_observation_aggregates_can_use_a_covering_index() {
    let Some(db) = TestDatabase::create("observation_covering_index").await else {
        return;
    };
    seed_observability_facts(&db.pool, Utc::now())
        .await
        .unwrap();
    let mut transaction = db.pool.begin().await.unwrap();
    // 小夹具只验证覆盖索引路径可达，不把耗时阈值或生产规划器成本固定在测试中
    sqlx::raw_sql(
        "create index observation_plain_cover_idx on model_requests (started_at)
           include (input_tokens, cost_amount) where recovered_at is null;
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
