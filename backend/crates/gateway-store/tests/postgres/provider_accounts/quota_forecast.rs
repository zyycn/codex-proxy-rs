//! 验证额度预测查询的请求筛选、配对采样与文档读取限制

use gateway_admin::model::observability as admin_observability;

use super::*;
use gateway_admin::model::quota_forecast_sampling::MAX_FORECAST_HISTORY_POINTS;
use gateway_store::postgres::{
    ObservabilityPageSize, ObservabilityRange, ProviderAccountUsageQuery, UsageRecordFilter,
    UsageRecordQuery,
};

#[tokio::test]
async fn quota_forecast_fractional_slots_keep_microsecond_edges_and_as_of_ties() {
    let Some(database) = TestDatabase::create("forecast_fractional_slots").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_fraction", "user-fraction"))
        .await
        .unwrap();
    let start = chrono::DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let duration = MAX_FORECAST_HISTORY_POINTS as i64 * 1_000_000 + 64;
    let end = start + TimeDelta::microseconds(duration);
    for (id, completed) in [
        ("before_edge", 1_000_000),
        ("after_edge", 1_000_001),
        ("as_of_a", duration),
        ("as_of_b", duration),
        ("pending", duration + 1),
    ] {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_fraction",
                provider_kind: "openai",
                model: "gpt-test",
                total_tokens: 100,
                cost_amount: "1",
                started_at: start,
            },
        )
        .await
        .unwrap();
        sqlx::query("update model_requests set completed_at=$2,provider_observation_json='{}'::jsonb where id=$1")
            .bind(id).bind(start+TimeDelta::microseconds(completed)).execute(&database.pool).await.unwrap();
    }
    let history = admin_account_store(&database.pool)
        .load_quota_forecast_history(&AccountUsageWindowQuery {
            account_id: "acct_fraction".to_owned(),
            key: "fraction".to_owned(),
            range: TimeRange { start, end },
        })
        .await
        .unwrap();
    assert_eq!(
        history
            .points
            .iter()
            .map(|point| point.usage.request_count)
            .collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    assert_eq!(history.usage.request_count, 4);
    assert_eq!(history.usage.tokens, 400);
    assert_eq!(history.pending_request_count, 1);
    database.close().await;
}

#[tokio::test]
async fn quota_forecast_excludes_openai_prewarm_but_preserves_inference_and_audit() {
    let Some(database) = TestDatabase::create("forecast_prewarm").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_prewarm", "user-prewarm"))
        .await
        .unwrap();
    let start = Utc::now() - TimeDelta::minutes(30);
    let end = start + TimeDelta::minutes(20);
    for (id, minute, tokens) in [("normal", 1, 100), ("review", 2, 200), ("prewarm", 3, 400)] {
        // 普通成功推理也可以只有输入 Token，不能用 output_tokens = 0 判定预热
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_prewarm",
                provider_kind: "openai",
                model: "gpt-test",
                total_tokens: tokens,
                cost_amount: "1",
                started_at: start + TimeDelta::minutes(minute),
            },
        )
        .await
        .unwrap();
    }
    sqlx::query("update model_requests set request_kind = 'review' where id = 'review'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query(
        "update model_requests set request_kind = 'prewarm', client_transport = 'websocket',
          client_status_code = null, upstream_transport = 'websocket', service_tier = 'auto',
          cost_source = 'unavailable', cost_amount = null, cost_currency = null,
          provider_observation_json = '{\"upstreamServiceTier\":\"auto\"}'::jsonb
         where id = 'prewarm'",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    let store = admin_account_store(&database.pool);
    let query = AccountUsageWindowQuery {
        account_id: "acct_prewarm".to_owned(),
        key: "week".to_owned(),
        range: TimeRange { start, end },
    };
    let history = store.load_quota_forecast_history(&query).await.unwrap();
    assert_eq!(history.usage.request_count, 2);
    assert_eq!(history.usage.tokens, 300);
    assert_eq!(history.usage.missing_token_count, 0);
    assert_eq!(history.usage.known_cost_count, 2);
    assert_eq!(history.usage.unavailable_cost_count, 0);
    assert_eq!(history.usage.usd, 2.0);
    assert_eq!(history.usage.excluded_request_count, 1);
    assert_eq!(history.pending_request_count, 0);
    // 预热响应携带的额度观测仍可配对，但它不增加累计推理用量
    assert_eq!(history.points.len(), 1);
    assert_eq!(history.points[0].usage, history.usage);

    super::super::drain_analytics(&database.pool).await;
    let repository = super::super::observability_repository(&database.pool);
    let range = ObservabilityRange::new(start, end).unwrap();
    let account_usage = repository
        .provider_account_usage(
            ProviderAccountUsageQuery::for_accounts(range, vec!["acct_prewarm".to_owned()])
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(account_usage[0].request_count, 2);
    assert_eq!(account_usage[0].cost_coverage.unavailable_count, 0);
    let records = repository
        .list_usage_records(UsageRecordQuery {
            status: Default::default(),
            range,
            filter: UsageRecordFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(records.total, 2);
    assert!(records.items.iter().all(|record| record.id != "prewarm"));
    let detail = repository.usage_record_detail("prewarm").await.unwrap();
    assert_eq!(detail.request.request_kind.as_deref(), Some("prewarm"));
    assert_eq!(detail.request.service_tier.as_deref(), Some("auto"));
    let dashboard = repository
        .dashboard_summary(
            admin_observability::DashboardQuery::new(admin_observability::TimeRange {
                start: range.start,
                end: range.end,
            }),
            end,
        )
        .await
        .unwrap();
    assert_eq!(dashboard.totals.request_count, 3);
    assert_eq!(dashboard.totals.total_tokens, 300);

    // 真正推理的未知费用仍必须阻止费用外推，不能靠放宽覆盖门槛掩盖缺失
    sqlx::query(
        "update model_requests set cost_source = 'unavailable', cost_amount = null,
          cost_currency = null, service_tier = 'auto' where id = 'review'",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    let history = store.load_quota_forecast_history(&query).await.unwrap();
    assert_eq!(history.usage.request_count, 2);
    assert_eq!(history.usage.known_cost_count, 1);
    assert_eq!(history.usage.unavailable_cost_count, 1);
    assert_eq!(history.usage.usd, 1.0);
    assert_eq!(history.usage.excluded_request_count, 1);
    database.close().await;
}

#[tokio::test]
async fn quota_forecast_does_not_apply_openai_prewarm_semantics_to_other_providers() {
    let Some(database) = TestDatabase::create("forecast_xai_kind").await else {
        return;
    };
    let mut candidate = account("acct_xai_kind", "user-xai-kind");
    candidate.provider_kind = "xai".to_owned();
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(candidate)
        .await
        .unwrap();
    let start = Utc::now() - TimeDelta::minutes(30);
    seed_model_request(
        &database.pool,
        ModelRequestSeed {
            request_id: "xai_kind",
            account_id: "acct_xai_kind",
            provider_kind: "xai",
            model: "grok-test",
            total_tokens: 100,
            cost_amount: "1",
            started_at: start,
        },
    )
    .await
    .unwrap();
    sqlx::query("update model_requests set request_kind = 'prewarm' where id = 'xai_kind'")
        .execute(&database.pool)
        .await
        .unwrap();
    let history = admin_account_store(&database.pool)
        .load_quota_forecast_history(&AccountUsageWindowQuery {
            account_id: "acct_xai_kind".to_owned(),
            key: "week".to_owned(),
            range: TimeRange {
                start,
                end: start + TimeDelta::minutes(10),
            },
        })
        .await
        .unwrap();
    assert_eq!(history.usage.request_count, 1);
    assert_eq!(history.usage.tokens, 100);
    assert_eq!(history.usage.known_cost_count, 1);
    assert_eq!(history.usage.usd, 1.0);
    assert_eq!(history.usage.excluded_request_count, 0);
    database.close().await;
}

#[tokio::test]
async fn quota_forecast_pairs_completed_usage_and_preserves_missing_pending_and_excluded_counts() {
    let Some(database) = TestDatabase::create("quota_forecast_pairing").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_forecast", "user-forecast"))
        .await
        .unwrap();
    let start = "2026-09-12T00:00:00Z"
        .parse::<chrono::DateTime<Utc>>()
        .unwrap();
    let end = start + TimeDelta::minutes(30);
    for (id, minute, tokens) in [
        ("before", -1, 999),
        ("start", 0, 100),
        ("tie_a", 5, 200),
        ("tie_b", 5, 300),
        ("missing", 10, 400),
        ("eur", 15, 500),
        ("late", 20, 600),
        ("cancelled", 21, 0),
        ("websocket", 22, 700),
        ("http", 23, 800),
        ("after", 30, 999),
    ] {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_forecast",
                provider_kind: "openai",
                model: "gpt-test",
                total_tokens: tokens,
                cost_amount: "1",
                started_at: start + TimeDelta::minutes(minute),
            },
        )
        .await
        .unwrap();
    }
    sqlx::query(
        "update model_requests set provider_observation_json =
         '{\"privateMarker\":\"never-debug-this\",\"rateLimitHeaders\":[]}'::jsonb",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query(
        "update model_requests set total_tokens = null, input_tokens = null,
          output_tokens = null, cost_source = 'unavailable', cost_amount = null, cost_currency = null
         where id = 'missing'",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query("update model_requests set cost_currency = 'EUR' where id = 'eur'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set completed_at = $1 where id = 'late'")
        .bind(end + TimeDelta::seconds(1))
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set outcome = 'cancelled' where id = 'cancelled'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query(
        "update model_requests set client_status_code = null,
          client_transport = case when id = 'websocket' then 'websocket' else 'http_sse' end
         where id in ('websocket', 'http')",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    let store = admin_account_store(&database.pool);
    let history = store
        .load_quota_forecast_history(&AccountUsageWindowQuery {
            account_id: "acct_forecast".to_owned(),
            key: "week".to_owned(),
            range: TimeRange { start, end },
        })
        .await
        .expect("forecast history");
    assert_eq!(history.usage.request_count, 6);
    assert_eq!(history.usage.tokens, 1_800);
    assert_eq!(history.usage.missing_token_count, 1);
    assert_eq!(history.usage.known_cost_count, 4);
    assert_eq!(history.usage.unavailable_cost_count, 2);
    assert_eq!(history.usage.usd, 4.0);
    assert_eq!(history.usage.excluded_request_count, 2);
    assert_eq!(history.pending_request_count, 1);
    assert!(history.points.iter().all(|point| point.completed_at <= end));
    let tie = history
        .points
        .iter()
        .find(|point| point.completed_at == start + TimeDelta::minutes(5) + TimeDelta::seconds(1))
        .unwrap();
    assert_eq!(tie.usage.tokens, 600);
    assert_eq!(tie.usage.request_count, 3);
    assert!(!format!("{history:?}").contains("never-debug-this"));
    let empty = store
        .load_quota_forecast_history(&AccountUsageWindowQuery {
            account_id: "acct_forecast".to_owned(),
            key: "empty".to_owned(),
            range: TimeRange {
                start: end + TimeDelta::hours(1),
                end: end + TimeDelta::hours(2),
            },
        })
        .await
        .unwrap();
    assert_eq!(empty.usage.request_count, 0);
    assert!(empty.points.is_empty());
    database.close().await;
}

#[tokio::test]
async fn quota_forecast_bounds_documents_without_sampling_away_token_totals() {
    let Some(database) = TestDatabase::create("quota_forecast_bound").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_forecast_bound", "user-forecast-bound"))
        .await
        .unwrap();
    let start = Utc::now() - TimeDelta::hours(6);
    for index in 0..256 {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: &format!("forecast_{index}"),
                account_id: "acct_forecast_bound",
                provider_kind: "openai",
                model: "gpt-test",
                total_tokens: 100,
                cost_amount: "0",
                started_at: start + TimeDelta::minutes(index),
            },
        )
        .await
        .unwrap();
    }
    sqlx::query("update model_requests set provider_observation_json = '{}'::jsonb")
        .execute(&database.pool)
        .await
        .unwrap();
    let history = admin_account_store(&database.pool)
        .load_quota_forecast_history(&AccountUsageWindowQuery {
            account_id: "acct_forecast_bound".to_owned(),
            key: "week".to_owned(),
            range: TimeRange {
                start,
                end: start + TimeDelta::minutes(256),
            },
        })
        .await
        .unwrap();
    assert_eq!(history.usage.request_count, 256);
    assert_eq!(history.usage.tokens, 25_600);
    assert_eq!(history.points.len(), MAX_FORECAST_HISTORY_POINTS);
    assert_eq!(history.points.last().unwrap().usage.tokens, 25_600);
    database.close().await;
}

#[tokio::test]
async fn quota_forecast_carries_unsampled_usage_between_document_points() {
    let Some(database) = TestDatabase::create("quota_forecast_bucket_prefix").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_prefix", "user-prefix"))
        .await
        .unwrap();
    let start = Utc::now() - TimeDelta::hours(6);
    let end = start + TimeDelta::minutes(MAX_FORECAST_HISTORY_POINTS as i64);
    for (id, second, document) in [
        ("before_doc", 10, false),
        ("first_doc", 20, true),
        ("tie_doc", 20, true),
        ("after_doc", 40, false),
        ("no_document_bucket", 70, false),
        ("third_doc", 130, true),
        ("after_last_doc", 150, false),
        ("late", 180, true),
    ] {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: id,
                account_id: "acct_prefix",
                provider_kind: "openai",
                model: "gpt-test",
                total_tokens: 100,
                cost_amount: "1",
                started_at: start + TimeDelta::seconds(second),
            },
        )
        .await
        .unwrap();
        if document {
            sqlx::query(
                "update model_requests set provider_observation_json = '{}'::jsonb where id = $1",
            )
            .bind(id)
            .execute(&database.pool)
            .await
            .unwrap();
        }
    }
    sqlx::query("update model_requests set completed_at = $1 where id = 'late'")
        .bind(end + TimeDelta::seconds(1))
        .execute(&database.pool)
        .await
        .unwrap();
    let history = admin_account_store(&database.pool)
        .load_quota_forecast_history(&AccountUsageWindowQuery {
            account_id: "acct_prefix".to_owned(),
            key: "week".to_owned(),
            range: TimeRange { start, end },
        })
        .await
        .unwrap();
    assert_eq!(history.points.len(), 2);
    assert_eq!(history.points[0].usage.request_count, 3);
    assert_eq!(history.points[0].usage.tokens, 300);
    assert_eq!(history.points[1].usage.request_count, 6);
    assert_eq!(history.points[1].usage.tokens, 600);
    assert_eq!(history.usage.request_count, 7);
    assert_eq!(history.usage.tokens, 700);
    assert_eq!(history.pending_request_count, 1);
    database.close().await;
}

#[tokio::test]
async fn quota_forecast_summaries_match_source_during_moves_deletes_and_rebuilds() {
    use super::super::drain_analytics;
    use gateway_admin::model::quota_forecast_sampling::QuotaForecastHistory;

    fn assert_same(actual: &QuotaForecastHistory, expected: &QuotaForecastHistory) {
        assert_eq!(actual.usage, expected.usage);
        assert_eq!(actual.pending_request_count, expected.pending_request_count);
        assert_eq!(actual.points.len(), expected.points.len());
        for (actual, expected) in actual.points.iter().zip(&expected.points) {
            assert_eq!(actual.started_at, expected.started_at);
            assert_eq!(actual.completed_at, expected.completed_at);
            assert_eq!(actual.usage, expected.usage);
        }
    }

    let Some(database) = TestDatabase::create("forecast_completion_projection").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_forecast_projection", "acct_forecast_moved"] {
        repository
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
    }
    let start = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for index in 0..200 {
        seed_model_request(
            &database.pool,
            ModelRequestSeed {
                request_id: &format!("forecast_projection_{index}"),
                account_id: "acct_forecast_projection",
                provider_kind: "openai",
                model: "gpt-test",
                total_tokens: index + 10,
                cost_amount: "0.125",
                started_at: start + TimeDelta::minutes(index * 3),
            },
        )
        .await
        .unwrap();
    }
    sqlx::query("update model_requests set provider_observation_json='{}'::jsonb where substring(id from '[0-9]+$')::int % 9=0")
        .execute(&database.pool).await.unwrap();
    // 完成时间与开始时间跨越多个桶，包含未来完成、缺失 Token、未知费用与恢复记录
    sqlx::query("update model_requests set completed_at=completed_at+interval '90 minutes' where substring(id from '[0-9]+$')::int % 7=0")
        .execute(&database.pool).await.unwrap();
    sqlx::query("update model_requests set total_tokens=null,input_tokens=null,cost_source='unavailable',cost_amount=null,cost_currency=null where id='forecast_projection_31'")
        .execute(&database.pool).await.unwrap();
    sqlx::query(
        "update model_requests set recovered_at=completed_at,recovery_request_id='forecast_projection_48',recovery_attempt_count=1,request_observation_json=request_observation_json || '{\"recovery\":{\"retryDelayMs\":0,\"totalLatencyMs\":1}}'::jsonb where id='forecast_projection_47'",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query("update model_requests set outcome='cancelled' where id='forecast_projection_63'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set cost_currency='EUR' where id='forecast_projection_83'")
        .execute(&database.pool)
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let queries = [
        AccountUsageWindowQuery {
            account_id: "acct_forecast_projection".to_owned(),
            key: "complete".to_owned(),
            range: TimeRange {
                start,
                end: start + TimeDelta::hours(10),
            },
        },
        AccountUsageWindowQuery {
            account_id: "acct_forecast_projection".to_owned(),
            key: "edges".to_owned(),
            range: TimeRange {
                start: start + TimeDelta::microseconds(1),
                end: start + TimeDelta::minutes(593) + TimeDelta::microseconds(1),
            },
        },
        AccountUsageWindowQuery {
            account_id: "acct_forecast_moved".to_owned(),
            key: "moved".to_owned(),
            range: TimeRange {
                start,
                end: start + TimeDelta::hours(10),
            },
        },
    ];
    let mut source = Vec::new();
    for query in &queries {
        source.push(store.load_quota_forecast_history(query).await.unwrap());
    }
    drain_analytics(&database.pool).await;
    for (query, expected) in queries.iter().zip(&source) {
        assert_same(
            &store.load_quota_forecast_history(query).await.unwrap(),
            expected,
        );
    }
    sqlx::query("update model_requests set provider_account_ref='acct_forecast_moved',started_at=$1,completed_at=$1+interval '1 second',total_tokens=1234,cost_amount=2.5 where id='forecast_projection_72'")
        .bind(start+TimeDelta::minutes(40)).execute(&database.pool).await.unwrap();
    sqlx::query("delete from model_requests where id='forecast_projection_47'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set completed_at=$1 where id='forecast_projection_196'")
        .bind(start + TimeDelta::minutes(590))
        .execute(&database.pool)
        .await
        .unwrap();
    let mut pending = Vec::new();
    for query in &queries {
        pending.push(store.load_quota_forecast_history(query).await.unwrap());
    }
    assert_eq!(pending[2].usage.request_count, 1);
    assert_eq!(pending[2].usage.tokens, 1234);
    drain_analytics(&database.pool).await;
    for (query, expected) in queries.iter().zip(&pending) {
        assert_same(
            &store.load_quota_forecast_history(query).await.unwrap(),
            expected,
        );
    }
    // 显式重建中的区间由窄事实接管，发布后仍与同一源结果一致
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) select distinct date_bin(interval '15 minutes',started_at,timestamptz '2000-01-01 UTC'),now() from request_analytics_facts")
        .execute(&database.pool).await.unwrap();
    for (query, expected) in queries.iter().zip(&pending) {
        assert_same(
            &store.load_quota_forecast_history(query).await.unwrap(),
            expected,
        );
    }
    drain_analytics(&database.pool).await;
    for (query, expected) in queries.iter().zip(&pending) {
        assert_same(
            &store.load_quota_forecast_history(query).await.unwrap(),
            expected,
        );
    }
    database.close().await;
}
