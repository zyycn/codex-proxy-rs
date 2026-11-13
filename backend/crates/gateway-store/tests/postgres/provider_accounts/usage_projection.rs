//! 验证账号同步用量在汇总、待投影修改和删除之间保持源事实一致

use super::super::drain_analytics;
use super::*;

#[tokio::test]
async fn quota_windows_reconcile_moved_requests_and_pending_deletions_without_waiting_for_worker() {
    let Some(database) = TestDatabase::create("account_window_source_reconciliation").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_usage_before", "acct_usage_after"] {
        repository
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
    }
    let start = chrono::DateTime::from_timestamp(Utc::now().timestamp().div_euclid(3600) * 3600, 0)
        .unwrap()
        - TimeDelta::hours(2);
    for (id, offset, tokens, cost) in [("usage_stays", 5, 11, "0"), ("usage_moves", 40, 23, "2.5")]
    {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_usage_before",
                provider_kind: "openai",
                model: "old-model",
                total_tokens: tokens,
                cost_amount: cost,
                started_at: start + TimeDelta::minutes(offset),
            },
        )
        .await
        .unwrap();
    }
    sqlx::query("update model_requests set cached_input_tokens=null,reasoning_output_tokens=null where id in ('usage_stays','usage_moves')")
        .execute(&database.pool).await.unwrap();
    drain_analytics(&database.pool).await;
    let windows = [
        AccountUsageWindowQuery {
            account_id: "acct_usage_before".to_owned(),
            key: "old".to_owned(),
            range: TimeRange {
                start,
                end: start + TimeDelta::hours(1),
            },
        },
        AccountUsageWindowQuery {
            account_id: "acct_usage_after".to_owned(),
            key: "new".to_owned(),
            range: TimeRange {
                start: start + TimeDelta::hours(1),
                end: start + TimeDelta::hours(2),
            },
        },
    ];
    let store = admin_account_store(&database.pool);
    let before = store.load_account_usage_by_windows(&windows).await.unwrap();
    let initial = before.iter().find(|usage| usage.key == "old").unwrap();
    assert_eq!(initial.usage.total_tokens, Some(34));
    assert_eq!(initial.usage.cached_input_tokens, None);
    assert_eq!(initial.usage.costs[0].amount.as_str(), "2.5");
    let ranked = store
        .list_accounts(
            AccountListQuery {
                page: 1,
                page_size: PageSize::new(1).unwrap(),
                provider_kind: None,
                group_filter: None,
                search: None,
                status: None,
                sort: Some(AccountSort {
                    field: AccountSortField::Usage,
                    direction: SortDirection::Desc,
                }),
            },
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(ranked.items[0].account.id, "acct_usage_before");

    sqlx::query("update model_requests set provider_account_ref='acct_usage_after',upstream_model_id='new-model',total_tokens=null,input_tokens=7,output_tokens=8,cached_input_tokens=0,cost_amount=4.75,started_at=$1,completed_at=$1+interval '1 minute',downstream_committed_at=$1+interval '1 minute',recovery_lease_expires_at=$1+interval '2 minutes' where id='usage_moves'")
        .bind(start+TimeDelta::minutes(70)).execute(&database.pool).await.unwrap();
    let changed = store.load_account_usage_by_windows(&windows).await.unwrap();
    let old = changed.iter().find(|usage| usage.key == "old").unwrap();
    let new = changed.iter().find(|usage| usage.key == "new").unwrap();
    assert_eq!(old.usage.request_count, 1);
    assert_eq!(old.usage.total_tokens, Some(11));
    assert_eq!(old.usage.last_used_at, Some(start + TimeDelta::minutes(5)));
    assert_eq!(
        old.usage.costs[0].amount.as_str(),
        "0",
        "a real zero-cost request retains its currency"
    );
    assert_eq!(new.usage.request_count, 1);
    assert_eq!(
        new.usage.total_tokens,
        Some(15),
        "quota windows retain component fallback for missing provider total"
    );
    assert_eq!(new.usage.cached_input_tokens, Some(0));
    assert_eq!(new.usage.reasoning_output_tokens, None);
    assert_eq!(new.usage.models[0].model, "new-model");
    assert_eq!(new.usage.costs[0].amount.as_str(), "4.75");
    sqlx::query("update runtime_settings set usage_retention_days=$1")
        .bind(i64::from(u32::MAX))
        .execute(&database.pool)
        .await
        .unwrap();
    for field in [AccountSortField::Usage, AccountSortField::LastUsedAt] {
        let ranked = store
            .list_accounts(
                AccountListQuery {
                    page: 1,
                    page_size: PageSize::new(1).unwrap(),
                    provider_kind: None,
                    group_filter: None,
                    search: None,
                    status: None,
                    sort: Some(AccountSort {
                        field,
                        direction: SortDirection::Desc,
                    }),
                },
                Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(ranked.items[0].account.id, "acct_usage_after");
    }

    // 独立源表真值不经过同一汇总代码，防止仅比较两个相同错误的读取分支
    let truth:(i64,i64,String)=sqlx::query_as("select count(*)::bigint,sum(coalesce(total_tokens,input_tokens+output_tokens))::bigint,sum(cost_amount)::text from model_requests where provider_account_ref='acct_usage_after' and started_at >= $1 and started_at < $2")
        .bind(windows[1].range.start).bind(windows[1].range.end).fetch_one(&database.pool).await.unwrap();
    assert_eq!(truth.0, new.usage.request_count as i64);
    assert_eq!(truth.1, new.usage.total_tokens.unwrap() as i64);
    assert_eq!(
        truth.2.parse::<gateway_store::DecimalAmount>().unwrap(),
        new.usage.costs[0].amount
    );
    sqlx::query("delete from model_requests where id='usage_stays'")
        .execute(&database.pool)
        .await
        .unwrap();
    let deleted = store.load_account_usage_by_windows(&windows).await.unwrap();
    let old = deleted.iter().find(|usage| usage.key == "old").unwrap();
    assert_eq!(old.usage.request_count, 0);
    assert_eq!(old.usage.last_used_at, None);
    assert!(old.usage.models.is_empty());
    assert!(old.usage.costs.is_empty());
    database.close().await;
}

#[tokio::test]
async fn model_success_rates_preserve_usage_across_source_summary_and_pending_reads() {
    let Some(database) = TestDatabase::create("account_model_health").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_health", "user-health"))
        .await
        .unwrap();
    let start = chrono::DateTime::from_timestamp(Utc::now().timestamp().div_euclid(3600) * 3600, 0)
        .unwrap()
        - TimeDelta::hours(3);
    for (index, (id, model, outcome, status)) in [
        ("health_success", "mixed", "succeeded", 200),
        ("health_retry", "mixed", "succeeded", 200),
        ("health_failed", "mixed", "failed", 502),
        ("health_incomplete", "mixed", "incomplete", 200),
        ("health_cancelled", "mixed", "cancelled", 200),
        ("health_caller", "mixed", "failed", 400),
        ("health_recovered", "mixed", "failed", 502),
        ("health_only_failed", "failed-only", "failed", 502),
        ("health_only_cancelled", "no-sample", "cancelled", 200),
        ("health_only_caller", "no-sample", "failed", 400),
    ]
    .into_iter()
    .enumerate()
    {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_health",
                provider_kind: "openai",
                model,
                total_tokens: 11,
                cost_amount: "2.5",
                started_at: start + TimeDelta::minutes(5 + index as i64 * 5),
            },
        )
        .await
        .unwrap();
        sqlx::query("update model_requests set outcome=$2,client_status_code=$3,attempt_count=case when id='health_retry' then 3 else 1 end where id=$1")
            .bind(id).bind(outcome).bind(status).execute(&database.pool).await.unwrap();
    }
    sqlx::query("update model_requests set recovery_request_id='health_retry',recovered_at=completed_at,recovery_attempt_count=1,request_observation_json=jsonb_set(request_observation_json,'{recovery}',jsonb_build_object('retryDelayMs',0,'totalLatencyMs',1)) where id='health_recovered'")
        .execute(&database.pool).await.unwrap();
    let windows = [
        AccountUsageWindowQuery {
            account_id: "acct_health".to_owned(),
            key: "whole".to_owned(),
            range: TimeRange {
                start,
                end: start + TimeDelta::hours(2),
            },
        },
        AccountUsageWindowQuery {
            account_id: "acct_health".to_owned(),
            key: "boundary".to_owned(),
            range: TimeRange {
                start: start + TimeDelta::minutes(3),
                end: start + TimeDelta::minutes(117),
            },
        },
    ];
    let store = admin_account_store(&database.pool);
    let source = store.load_account_usage_by_windows(&windows).await.unwrap();
    for result in &source {
        let usage = &result.usage;
        assert_eq!((usage.request_count, usage.total_tokens), (2, Some(22)));
        assert_eq!(usage.costs[0].amount.as_str(), "5");
        assert_eq!(usage.cost_coverage.provider_reported_count, 2);
        let mixed = usage
            .models
            .iter()
            .find(|model| model.model == "mixed")
            .unwrap();
        assert_eq!(mixed.health.success_rate(), Some(50.0));
        assert_eq!(mixed.health.success_requests, 2);
        assert_eq!(mixed.health.failed_requests, 1);
        assert_eq!(mixed.health.incomplete_requests, 1);
        let failed = usage
            .models
            .iter()
            .find(|model| model.model == "failed-only")
            .unwrap();
        assert_eq!(failed.health.success_rate(), Some(0.0));
        assert_eq!(failed.last_used_at, None);
        assert!(failed.costs.is_empty());
        let excluded = usage
            .models
            .iter()
            .find(|model| model.model == "no-sample")
            .unwrap();
        assert_eq!(excluded.health.success_rate(), None);
    }
    drain_analytics(&database.pool).await;
    assert_eq!(
        store.load_account_usage_by_windows(&windows).await.unwrap(),
        source
    );
    let rolling = store
        .load_account_usage(windows[0].range, &["acct_health".to_owned()])
        .await
        .unwrap();
    for model in &rolling[0].models {
        assert_eq!(
            model.health,
            source[0]
                .usage
                .models
                .iter()
                .find(|expected| expected.model == model.model)
                .unwrap()
                .health
        );
    }

    // 待投影的成功变失败必须撤销费用与最近使用时间，失败变成功则补入当前源版本
    sqlx::query("update model_requests set outcome='failed',client_status_code=502 where id in ('health_success','health_retry')")
        .execute(&database.pool).await.unwrap();
    sqlx::query("update model_requests set outcome='succeeded',client_status_code=200 where id='health_only_failed'")
        .execute(&database.pool).await.unwrap();
    sqlx::query("delete from model_requests where id='health_failed'")
        .execute(&database.pool)
        .await
        .unwrap();
    let pending = store.load_account_usage_by_windows(&windows).await.unwrap();
    for result in &pending {
        assert_eq!(
            (result.usage.request_count, result.usage.total_tokens),
            (1, Some(11))
        );
        assert_eq!(result.usage.costs[0].amount.as_str(), "2.5");
        let mixed = result
            .usage
            .models
            .iter()
            .find(|model| model.model == "mixed")
            .unwrap();
        assert_eq!(mixed.health.success_rate(), Some(0.0));
        assert_eq!(mixed.health.failed_requests, 2);
        assert_eq!(mixed.last_used_at, None);
        assert!(mixed.costs.is_empty());
        let recovered = result
            .usage
            .models
            .iter()
            .find(|model| model.model == "failed-only")
            .unwrap();
        assert_eq!(recovered.health.success_rate(), Some(100.0));
    }
    drain_analytics(&database.pool).await;
    assert_eq!(
        store.load_account_usage_by_windows(&windows).await.unwrap(),
        pending
    );
    database.close().await;
}
