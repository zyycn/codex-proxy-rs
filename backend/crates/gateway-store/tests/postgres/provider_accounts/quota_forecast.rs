use super::*;
use gateway_admin::model::quota_forecast_sampling::MAX_FORECAST_HISTORY_POINTS;

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
