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
