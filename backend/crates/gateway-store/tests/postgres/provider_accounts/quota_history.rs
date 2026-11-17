//! 验证历史额度按账号与完成时间翻页，保留重置和最近有效基线

use gateway_admin::model::quota_history::{QuotaHistoryPage, QuotaHistoryQuery, QuotaReading};

use super::*;

#[tokio::test]
async fn quota_history_pages_sparse_tied_observations_through_the_nearest_baseline() {
    let Some(db) = TestDatabase::create("quota_history").await else {
        return;
    };
    let store = admin_account_store(&db.pool);
    let id = ProviderAccountId::new("acct_history").unwrap();
    let mut saved = account(id.as_str(), "history");
    saved.enabled = false;
    PgProviderAccountRepository::new(db.pool.clone())
        .insert_provider_account(saved)
        .await
        .unwrap();
    let record = store.load_account_record(&id).await.unwrap().unwrap();
    let created = record.created_at;
    let provider = record.provider_kind;
    let start = chrono::DateTime::from_timestamp(created.timestamp() + 3600, 0).unwrap();
    let range = TimeRange::new(start, start + TimeDelta::minutes(10)).unwrap();
    let tied = start + TimeDelta::microseconds(1_123_456);
    let before_reset = start.timestamp() + 1;
    let after_reset = before_reset + 604800;
    let weekly = |percent, reset| json!({"weekly":[percent,reset]});
    let row = |key: String, at, doc| (key, "acct_history", "openai", at, doc);

    // 同一微秒的请求跨页时，继续扫描才能找到其中唯一的同桶读数
    let mut rows: Vec<_> = (0..66)
        .map(|i| {
            row(
                format!("tied_{i:02}"),
                tied,
                if i == 0 {
                    weekly(3, after_reset)
                } else {
                    json!({})
                },
            )
        })
        .collect();
    rows.extend([
        row("start".into(), start, weekly(96, before_reset)),
        row("end".into(), range.end, weekly(90, after_reset)),
        row(
            "wrong_window".into(),
            start - TimeDelta::microseconds(1),
            json!({"daily":[60,before_reset]}),
        ),
        row(
            "baseline".into(),
            start - TimeDelta::minutes(1),
            weekly(94, before_reset),
        ),
        row(
            "older".into(),
            start - TimeDelta::minutes(2),
            weekly(80, before_reset),
        ),
        row(
            "outside_lookback".into(),
            start - TimeDelta::minutes(11),
            json!({"unknown":[75,before_reset]}),
        ),
        (
            "other".into(),
            "acct_other",
            "openai",
            tied,
            weekly(90, after_reset),
        ),
        (
            "other_provider".into(),
            "acct_history",
            "other",
            tied,
            weekly(88, after_reset),
        ),
    ]);
    for (request_id, account_id, provider_kind, at, doc) in rows {
        seed_model_request(
            &db.pool,
            ModelRequestSeed {
                request_id: &request_id,
                account_id,
                provider_kind,
                model: "gpt-test",
                total_tokens: 1,
                cost_amount: "1",
                started_at: at - TimeDelta::seconds(1),
            },
        )
        .await
        .unwrap();
        sqlx::query("update model_requests set provider_observation_json=$2 where id=$1")
            .bind(request_id)
            .bind(doc)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    for key in ["weekly", "unknown"] {
        let mut query = QuotaHistoryQuery {
            window_key: key.into(),
            range,
            cursor: None,
        };
        let scan = TimeRange {
            start: query.scan_start(),
            end: range.end,
        };
        let mut samples = Vec::new();
        let mut pages = 0;
        let baseline = loop {
            let raw = store
                .quota_documents(&id, &provider, created, scan, query.cursor.as_ref())
                .await
                .unwrap();
            let page = QuotaHistoryPage::from_documents(raw, &query, |doc, key| {
                let pair = doc.expose_to_provider().expose_to_provider().get(key)?;
                Some(QuotaReading {
                    used_percent: pair[0].as_f64()?,
                    reset_at: chrono::DateTime::from_timestamp(pair[1].as_i64()?, 0),
                    plan_type: None,
                })
            });
            pages += 1;
            if pages == 1 {
                assert!(page.samples.is_empty() && page.next_cursor.is_some());
            }
            samples.extend(page.samples.into_iter().map(|p| {
                (
                    p.observed_at,
                    p.reading.used_percent,
                    p.reading.reset_at.unwrap().timestamp(),
                )
            }));
            query.cursor = page.next_cursor;
            if query.cursor.is_none() {
                break page.baseline;
            }
            assert!(pages < 4, "游标未前进");
        };
        if key == "weekly" {
            assert_eq!(
                samples,
                [(tied, 3.0, after_reset), (start, 96.0, before_reset)]
            );
            let base = baseline.unwrap();
            assert_eq!(
                (base.observed_at, base.reading.used_percent),
                (start - TimeDelta::minutes(1), 94.0)
            );
        } else {
            assert!(samples.is_empty() && baseline.is_none());
        }
    }
    db.close().await;
}
