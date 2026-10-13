//! 请求列表的精确总数、双向分页与筛选隔离

use chrono::{TimeDelta, Utc};
use gateway_store::postgres::{
    ObservabilityPageSize, ObservabilityRange, OpsErrorFilter, OpsErrorQuery, UsageRecordFilter,
    UsageRecordQuery,
};

use super::{TestDatabase, analytics::seed, observability_repository};

#[tokio::test]
async fn projected_page_counts_correct_pending_moves_deletions_and_model_or() {
    let Some(database) = TestDatabase::create("usage_projected_pages").await else {
        return;
    };
    let start = chrono::DateTime::parse_from_rfc3339("2026-09-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for index in 0..40 {
        seed(
            &database.pool,
            &format!("projected_{index:02}"),
            start + TimeDelta::minutes(index),
            &[],
        )
        .await;
    }
    super::drain_analytics(&database.pool).await;
    let repository = observability_repository(&database.pool);
    for stage in 0..3 {
        if stage == 1 {
            sqlx::query("update model_requests set started_at=started_at+interval '30 minutes',recovery_lease_expires_at=recovery_lease_expires_at+interval '30 minutes',completed_at=completed_at+interval '30 minutes',requested_model_id='model_b',upstream_model_id='model_b' where id='projected_06'")
                .execute(&database.pool).await.unwrap();
            sqlx::query("delete from model_requests where id='projected_17'")
                .execute(&database.pool)
                .await
                .unwrap();
            sqlx::query("update model_requests set outcome='failed' where id='projected_21'")
                .execute(&database.pool)
                .await
                .unwrap();
            seed(
                &database.pool,
                "projected_new",
                start + TimeDelta::seconds(30),
                &[],
            )
            .await;
        } else if stage == 2 {
            super::drain_analytics(&database.pool).await;
        }
        for edge in [0, 31] {
            let range = ObservabilityRange::new(
                start + TimeDelta::seconds(edge),
                start + TimeDelta::hours(1) - TimeDelta::seconds(edge),
            )
            .unwrap();
            for model in [None, Some("model_a"), Some("model_b")] {
                // 真值只读取实际源版本，不依赖被测汇总或分页辅助函数
                let expected:Vec<String> = sqlx::query_scalar("select id from model_requests where started_at >= $1 and started_at < $2 and outcome='succeeded' and ($3::text is null or requested_model_id=$3 or upstream_model_id=$3) order by started_at desc,id desc")
                    .bind(range.start).bind(range.end).bind(model).fetch_all(&database.pool).await.unwrap();
                for current_page in [1, 3, 6, 8] {
                    let page = repository
                        .list_usage_records(UsageRecordQuery {
                            range,
                            filter: UsageRecordFilter {
                                client_api_key_ref: Some("key_analytics".to_owned()),
                                model: model.map(str::to_owned),
                                ..Default::default()
                            },
                            current_page,
                            page_size: ObservabilityPageSize::new(7).unwrap(),
                        })
                        .await
                        .unwrap();
                    assert_eq!(
                        page.total,
                        expected.len() as u64,
                        "stage={stage}, edge={edge}, model={model:?}"
                    );
                    assert_eq!(
                        page.items.iter().map(|row| &row.id).collect::<Vec<_>>(),
                        expected
                            .iter()
                            .skip((current_page as usize - 1) * 7)
                            .take(7)
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }
    database.close().await;
}

#[tokio::test]
async fn usage_pages_preserve_order_and_exact_totals_from_both_ends() {
    let Some(database) = TestDatabase::create("usage_bidirectional_pages").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    seed(&database.pool, "page_00", start, &[]).await;
    sqlx::query("insert into model_requests select cloned.* from model_requests original cross join generate_series(1,22) n cross join lateral jsonb_populate_record(null::model_requests,to_jsonb(original)||jsonb_build_object('id','page_'||lpad(n::text,2,'0'),'client_api_key_ref',case when n%2=0 then 'key_analytics' else 'other_key' end)) cloned where original.id='page_00'")
        .execute(&database.pool).await.unwrap();
    let range =
        ObservabilityRange::new(start - TimeDelta::seconds(1), start + TimeDelta::seconds(1))
            .unwrap();
    let repository = observability_repository(&database.pool);
    for key in [
        None,
        Some("key_analytics"),
        Some("other_key"),
        Some("absent_key"),
    ] {
        let expected = (0..23)
            .rev()
            .filter(|n| match key {
                None => true,
                Some("key_analytics") => n % 2 == 0,
                Some("other_key") => n % 2 != 0,
                Some(_) => false,
            })
            .map(|n| format!("page_{n:02}"))
            .collect::<Vec<_>>();
        for page in 1..=5 {
            let result = repository
                .list_usage_records(UsageRecordQuery {
                    range,
                    filter: UsageRecordFilter {
                        client_api_key_ref: key.map(str::to_owned),
                        ..Default::default()
                    },
                    current_page: page,
                    page_size: ObservabilityPageSize::new(7).unwrap(),
                })
                .await
                .unwrap();
            let ids = result
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>();
            let expected_page = expected
                .iter()
                .skip((page as usize - 1) * 7)
                .take(7)
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(
                result.total,
                expected.len() as u64,
                "key={key:?}, page={page}"
            );
            assert_eq!(ids, expected_page, "key={key:?}, page={page}");
        }
    }
    database.close().await;
}

#[tokio::test]
async fn usage_pages_cross_minutes_and_keep_exact_counts_after_source_changes() {
    let Some(database) = TestDatabase::create("usage_time_pages").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    for index in 0..29 {
        seed(
            &database.pool,
            &format!("cross_{index:02}"),
            start + TimeDelta::seconds((index / 3) * 100),
            &["group_page"],
        )
        .await;
    }
    let repository = observability_repository(&database.pool);
    let range = ObservabilityRange::new(start, start + TimeDelta::minutes(20)).unwrap();
    for removed in [None, Some("cross_16")] {
        if let Some(id) = removed {
            sqlx::query("delete from model_requests where id = $1")
                .bind(id)
                .execute(&database.pool)
                .await
                .unwrap();
        }
        let expected = (0..29)
            .rev()
            .map(|index| format!("cross_{index:02}"))
            .filter(|id| Some(id.as_str()) != removed)
            .collect::<Vec<_>>();
        for current_page in 1..=6 {
            let page = repository
                .list_usage_records(UsageRecordQuery {
                    range,
                    filter: UsageRecordFilter {
                        client_api_key_ref: Some("key_analytics".to_owned()),
                        search: Some("model_".to_owned()),
                        ..Default::default()
                    },
                    current_page,
                    page_size: ObservabilityPageSize::new(7).unwrap(),
                })
                .await
                .unwrap();
            assert_eq!(page.total, expected.len() as u64);
            assert_eq!(
                page.items.iter().map(|item| &item.id).collect::<Vec<_>>(),
                expected
                    .iter()
                    .skip((current_page as usize - 1) * 7)
                    .take(7)
                    .collect::<Vec<_>>(),
                "page={current_page}, removed={removed:?}",
            );
        }
    }
    database.close().await;
}

#[tokio::test]
async fn ops_pages_merge_sources_across_minutes_with_stable_ties() {
    let Some(database) = TestDatabase::create("ops_time_pages").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    let mut expected = Vec::new();
    for index in 0..17 {
        let id = format!("error_{index:02}");
        let at = start + TimeDelta::seconds((index / 3) * 100);
        seed(&database.pool, &id, at, &[]).await;
        sqlx::query("update model_requests set error_kind = 'upstream_error' where id = $1")
            .bind(&id)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into ops_events (id, model_request_id, attempt_index, level, component,
                 operation, failure_kind, message, created_at)
             values ($1, $1, 1, 'error', 'routing', 'responses', 'upstream_error', 'fixture', $2)",
        )
        .bind(&id)
        .bind(at + TimeDelta::minutes(1))
        .execute(&database.pool)
        .await
        .unwrap();
        for source in ["model_request", "ops_event"] {
            expected.push((at + TimeDelta::minutes(1), format!("{source}:{id}")));
        }
    }
    expected.sort_by(|a, b| b.cmp(a));
    let repository = observability_repository(&database.pool);
    let range = ObservabilityRange::new(start, start + TimeDelta::minutes(20)).unwrap();
    for current_page in 1..=7 {
        let page = repository
            .list_ops_errors(OpsErrorQuery {
                range,
                filter: OpsErrorFilter {
                    client_api_key_ref: Some("key_analytics".to_owned()),
                    search: Some("error_".to_owned()),
                    ..Default::default()
                },
                current_page,
                page_size: ObservabilityPageSize::new(7).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(page.total, expected.len() as u64);
        assert_eq!(
            page.items
                .iter()
                .map(|item| &item.stable_sort_id)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .skip((current_page as usize - 1) * 7)
                .take(7)
                .map(|(_, id)| id)
                .collect::<Vec<_>>(),
            "page={current_page}",
        );
    }
    database.close().await;
}
