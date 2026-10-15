//! 通用用量维护的原生统计、重试、原子性、重建与资源预算验收

use super::{TestDatabase, drain_analytics, observability_query_budget, observability_repository};
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::{
        auth::{AdminAuditEvent, AuditActorKind},
        usage_data::*,
    },
    ports::usage_data::*,
};
use gateway_store::postgres::{
    ObservabilityQueryBudget, ObservabilityRange, PgRequestAnalyticsRepository, PgUsageDataStore,
    UsageRecordFilter as NativeFilter,
};
use std::{collections::BTreeMap, time::Duration};

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}
fn filter() -> UsageRecordFilter {
    UsageRecordFilter {
        start_ms: start().timestamp_millis(),
        end_ms: (start() + TimeDelta::days(1)).timestamp_millis(),
        provider: None,
        model: None,
        account_ref: None,
        client_key_ref: None,
    }
}
fn audit() -> AdminAuditEvent {
    AdminAuditEvent {
        id: uuid::Uuid::new_v4().to_string(),
        actor_kind: AuditActorKind::System,
        actor_admin_user_id: None,
        actor_ref: "test".into(),
        request_id: None,
        action: "usage.put".into(),
        entity_kind: "usage".into(),
        entity_ref: "test".into(),
        config_revision: None,
        changed_fields: vec!["usage".into()],
        occurred_at: Utc::now(),
    }
}
fn records(changes: Vec<UsageRecordChange>) -> StoredUsageWrite {
    StoredUsageWrite {
        changes: changes.into_iter().map(|c| (c, audit())).collect(),
    }
}
fn aggregates(changes: Vec<UsageAggregateChange>) -> StoredAggregateWrite {
    StoredAggregateWrite {
        changes: changes.into_iter().map(|c| (c, audit())).collect(),
    }
}
fn data(total: u64) -> UsageRecordData {
    UsageRecordData {
        client_key_ref: "key_usage".into(),
        provider: Some("openai".into()),
        account_ref: Some("account_usage".into()),
        requested_model: Some("model_a".into()),
        model: Some("model_a".into()),
        protocol: "openai".into(),
        operation: "responses".into(),
        endpoint: "/v1/responses".into(),
        client_transport: "http_sse".into(),
        service_tier: None,
        started_at_ms: start().timestamp_millis(),
        completed_at_ms: Some((start() + TimeDelta::seconds(1)).timestamp_millis()),
        outcome: UsageOutcome::Succeeded,
        tokens: UsageTokens {
            input_tokens: Some(UsageCount(total)),
            total_tokens: Some(UsageCount(total)),
            ..Default::default()
        },
        cost: UsageCost::Unavailable,
    }
}
fn put(id: &str, total: u64) -> UsageRecordChange {
    UsageRecordChange::Put {
        id: id.into(),
        data: Box::new(data(total)),
    }
}
fn contribution(key: &str, requests: i64, tokens: i64, cost: &str) -> UsageAggregateChange {
    UsageAggregateChange::Put {
        aggregate: UsageAggregate {
            identity: UsageAggregateKey {
                bucket_start_ms: start().timestamp_millis(),
                key: key.into(),
                dimensions: UsageDimensions {
                    provider: Some("openai".into()),
                    model: Some("model_a".into()),
                    account_ref: Some("account_usage".into()),
                    client_key_ref: Some("key_usage".into()),
                    ..Default::default()
                },
            },
            counts: BTreeMap::from([
                (UsageMetric::RequestCount, UsageDelta(requests)),
                (UsageMetric::CompletedRequestCount, UsageDelta(requests)),
                (UsageMetric::TotalTokens, UsageDelta(tokens)),
            ]),
            costs: BTreeMap::from([("USD".into(), cost.into())]),
        },
    }
}

#[tokio::test]
async fn detail_crud_is_retry_safe_atomic_and_updates_native_statistics() {
    let Some(db) = TestDatabase::create("usage_detail_crud").await else {
        return;
    };
    let store = PgUsageDataStore::new(db.pool.clone(), observability_query_budget());
    store
        .write_records(records(vec![put("a", 10), put("b", 20)]))
        .await
        .unwrap();
    store
        .write_records(records(vec![put("a", 10)]))
        .await
        .unwrap();
    let first = store
        .list_records(ListUsageRecords {
            filter: filter(),
            cursor: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    let second = store
        .list_records(ListUsageRecords {
            filter: filter(),
            cursor: first.next_cursor,
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(first.items[0].id, "a");
    assert_eq!(second.items[0].id, "b");
    assert!(second.next_cursor.is_none());
    let mut invalid = data(50);
    invalid.cost = UsageCost::Known {
        source: UsageCostSource::Calculated,
        amount: "invalid".into(),
        currency: "USD".into(),
    };
    let err = store
        .write_records(records(vec![
            put("a", 99),
            UsageRecordChange::Put {
                id: "bad".into(),
                data: Box::new(invalid),
            },
        ]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, UsageErrorKind::InvalidInput);
    assert_eq!(
        store
            .get_record(GetUsageRecord { id: "a".into() })
            .await
            .unwrap()
            .data
            .tokens
            .total_tokens,
        Some(UsageCount(10))
    );
    drain_analytics(&db.pool).await;
    let repo = observability_repository(&db.pool);
    let range = ObservabilityRange::new(start(), start() + TimeDelta::days(1)).unwrap();
    let summary = repo
        .usage_summary(range, NativeFilter::default())
        .await
        .unwrap();
    assert_eq!(
        (
            summary.requests.request_count,
            summary.requests.total_tokens
        ),
        (2, 30)
    );
    store
        .write_records(records(vec![
            UsageRecordChange::Update {
                id: "a".into(),
                tokens: Some(data(100).tokens),
                cost: Some(UsageCost::Known {
                    source: UsageCostSource::Calculated,
                    amount: "0".into(),
                    currency: "USD".into(),
                }),
            },
            UsageRecordChange::Delete { id: "b".into() },
        ]))
        .await
        .unwrap();
    drain_analytics(&db.pool).await;
    let row = store
        .get_record(GetUsageRecord { id: "a".into() })
        .await
        .unwrap();
    assert!(matches!(row.data.cost, UsageCost::Known { .. }));
    assert!(row.billing_snapshot.is_none());
    let summary = repo
        .usage_summary(range, NativeFilter::default())
        .await
        .unwrap();
    assert_eq!(
        (
            summary.requests.request_count,
            summary.requests.total_tokens
        ),
        (1, 100)
    );
    assert!(
        store
            .get_record(GetUsageRecord { id: "b".into() })
            .await
            .is_err()
    );
    assert!(
        store
            .write_records(records(vec![UsageRecordChange::Delete { id: "b".into() }]))
            .await
            .unwrap()
            .changed_ids
            .is_empty()
    );
    db.close().await;
}

#[tokio::test]
async fn aggregate_import_and_signed_correction_survive_native_rebuild_without_schema_changes() {
    let Some(db) = TestDatabase::create("usage_aggregate_rebuild").await else {
        return;
    };
    let store = PgUsageDataStore::new(db.pool.clone(), observability_query_budget());
    store
        .write_records(records(vec![put("native", 120)]))
        .await
        .unwrap();
    drain_analytics(&db.pool).await;
    let imported = contribution("daily", 100, 1100, "2.75");
    store
        .write_aggregates(aggregates(vec![imported.clone()]))
        .await
        .unwrap();
    store
        .write_aggregates(aggregates(vec![
            imported,
            contribution("correction", -20, -100, "-1"),
        ]))
        .await
        .unwrap();
    let repo = observability_repository(&db.pool);
    let range = ObservabilityRange::new(start(), start() + TimeDelta::days(1)).unwrap();
    for _ in 0..2 {
        let summary = repo
            .usage_summary(range, NativeFilter::default())
            .await
            .unwrap();
        assert_eq!(
            (
                summary.requests.request_count,
                summary.requests.total_tokens
            ),
            (81, 1120)
        );
        let filtered = repo
            .usage_summary(
                range,
                NativeFilter {
                    provider_account_ref: Some("account_usage".into()),
                    provider_kind: Some("openai".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(filtered.requests.request_count, 81);
        use gateway_admin::{
            model::{accounts::AccountUsageWindowQuery, observability::TimeRange},
            ports::store::AccountStore,
        };
        let windows = [AccountUsageWindowQuery {
            account_id: "account_usage".into(),
            key: "window".into(),
            range: TimeRange {
                start: range.start,
                end: range.end,
            },
        }];
        let usage = super::admin_account_store(&db.pool)
            .load_account_usage_by_windows(&windows)
            .await
            .unwrap();
        assert_eq!(usage[0].usage.request_count, 81);
        assert_eq!(usage[0].usage.total_tokens, Some(1120));
        assert_eq!(usage[0].usage.costs[0].amount.as_str(), "1.75");

        let costs = repo
            .usage_diagnostics(
                range,
                NativeFilter::default(),
                gateway_store::postgres::DiagnosticDimension::Account,
                10,
            )
            .await
            .unwrap();
        assert_eq!(costs.total_request_count, 81);
        assert_eq!(costs.items[0].costs[0].amount.as_str(), "1.75");
        sqlx::query(
            "update request_analytics_build_state set schema_version=0,histogram_version=0",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        drain_analytics(&db.pool).await;
    }
    let page = store
        .list_aggregates(ListUsageAggregates {
            filter: filter(),
            cursor: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    let next = store
        .list_aggregates(ListUsageAggregates {
            filter: filter(),
            cursor: page.next_cursor,
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(page.items.len() + next.items.len(), 2);
    let count: i64 = sqlx::query_scalar("select count(*) from model_requests")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "aggregates must not fabricate request details");
    // 显式桶重建只重建请求投影，不能删除手工维护的统计贡献
    sqlx::query("insert into request_analytics_pending_buckets(bucket_start,oldest_pending_at) values($1,now()) on conflict(bucket_start) do update set change_version=nextval('request_analytics_change_version')").bind(start()).execute(&db.pool).await.unwrap();
    let worker = PgRequestAnalyticsRepository::new(db.pool.clone(), observability_query_budget());
    for _ in 0..30 {
        worker.maintain(500).await.unwrap();
    }
    assert_eq!(
        repo.usage_summary(range, NativeFilter::default())
            .await
            .unwrap()
            .requests
            .request_count,
        81
    );
    store
        .write_aggregates(aggregates(
            page.items
                .into_iter()
                .chain(next.items)
                .map(|a| UsageAggregateChange::Delete {
                    identity: a.identity,
                })
                .collect(),
        ))
        .await
        .unwrap();
    assert_eq!(
        repo.usage_summary(range, NativeFilter::default())
            .await
            .unwrap()
            .requests
            .request_count,
        1
    );
    db.close().await;
}

#[tokio::test]
async fn empty_native_history_still_shows_aggregates_and_bad_values_return_errors() {
    let Some(db) = TestDatabase::create("usage_aggregate_bounds").await else {
        return;
    };
    let store = PgUsageDataStore::new(db.pool.clone(), observability_query_budget());
    drain_analytics(&db.pool).await;
    store
        .write_aggregates(aggregates(vec![contribution("only", 7, 90, "1")]))
        .await
        .unwrap();
    let repo = observability_repository(&db.pool);
    let range = ObservabilityRange::new(start(), start() + TimeDelta::days(1)).unwrap();
    assert_eq!(
        repo.usage_summary(range, NativeFilter::default())
            .await
            .unwrap()
            .requests
            .request_count,
        7
    );
    store
        .write_aggregates(aggregates(vec![contribution("only", i64::MIN, 90, "1")]))
        .await
        .unwrap();
    assert!(
        repo.usage_summary(range, NativeFilter::default())
            .await
            .is_err()
    );
    store
        .write_aggregates(aggregates(vec![contribution("only", 8, 90, "1")]))
        .await
        .unwrap();
    assert_eq!(
        repo.usage_summary(range, NativeFilter::default())
            .await
            .unwrap()
            .requests
            .request_count,
        8
    );
    db.close().await;
}

#[tokio::test]
async fn timeout_and_cancellation_release_connections_and_rollback_the_whole_batch() {
    let Some(db) = TestDatabase::create("usage_budget").await else {
        return;
    };
    let budget = ObservabilityQueryBudget::try_new(
        1,
        Duration::from_millis(200),
        Duration::from_millis(300),
    )
    .unwrap();
    let store = PgUsageDataStore::new(db.pool.clone(), budget);
    store
        .write_records(records(vec![put("locked", 10)]))
        .await
        .unwrap();
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("select id from model_requests where id='locked' for update")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    assert_eq!(
        store
            .write_records(records(vec![put("first", 9), put("locked", 20)]))
            .await
            .unwrap_err()
            .kind,
        UsageErrorKind::Unavailable
    );
    lock.rollback().await.unwrap();
    assert_eq!(
        store
            .get_record(GetUsageRecord { id: "first".into() })
            .await
            .unwrap_err()
            .kind,
        UsageErrorKind::NotFound
    );
    assert_eq!(
        store
            .get_record(GetUsageRecord {
                id: "locked".into()
            })
            .await
            .unwrap()
            .data
            .tokens
            .total_tokens,
        Some(UsageCount(10))
    );
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("select id from model_requests where id='locked' for update")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let cloned = store.clone();
    let task = tokio::spawn(async move {
        cloned
            .write_records(records(vec![put("cancelled", 9), put("locked", 30)]))
            .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    task.abort();
    let _ = task.await;
    lock.rollback().await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        store
            .get_record(GetUsageRecord {
                id: "cancelled".into()
            })
            .await
            .unwrap_err()
            .kind,
        UsageErrorKind::NotFound
    );
    store
        .write_records(records(vec![put("after", 1)]))
        .await
        .unwrap();
    db.close().await;
}

#[tokio::test]
async fn unassigned_correction_changes_totals_without_inventing_dimension_members() {
    let Some(db) = TestDatabase::create("usage_global_correction").await else {
        return;
    };
    let store = PgUsageDataStore::new(db.pool.clone(), observability_query_budget());
    store
        .write_records(records(vec![put("a", 100), put("b", 100)]))
        .await
        .unwrap();
    drain_analytics(&db.pool).await;
    let mut correction = contribution("global", -1, -50, "0");
    let UsageAggregateChange::Put { aggregate } = &mut correction else {
        unreachable!()
    };
    aggregate.identity.dimensions = UsageDimensions::default();
    store
        .write_aggregates(aggregates(vec![correction]))
        .await
        .unwrap();
    let repo = observability_repository(&db.pool);
    let range = ObservabilityRange::new(start(), start() + TimeDelta::days(1)).unwrap();
    let summary = repo
        .usage_summary(range, NativeFilter::default())
        .await
        .unwrap();
    assert_eq!(
        (
            summary.requests.request_count,
            summary.requests.total_tokens
        ),
        (1, 150)
    );
    assert_eq!(summary.providers.len(), 1);
    assert_eq!(summary.providers[0].request_count, 2);
    db.close().await;
}

#[tokio::test]
async fn aggregate_corrections_participate_in_bounded_account_and_key_ranking() {
    let Some(db) = TestDatabase::create("usage_aggregate_ranking").await else {
        return;
    };
    let store = PgUsageDataStore::new(db.pool.clone(), observability_query_budget());
    let mut other = data(10);
    other.account_ref = Some("account_other".into());
    other.client_key_ref = "key_other".into();
    store
        .write_records(records(vec![
            put("a", 10),
            put("b", 10),
            UsageRecordChange::Put {
                id: "c".into(),
                data: Box::new(other),
            },
        ]))
        .await
        .unwrap();
    drain_analytics(&db.pool).await;
    let mut imported = contribution("other", 10, 100, "0");
    let UsageAggregateChange::Put { aggregate } = &mut imported else {
        unreachable!()
    };
    aggregate.identity.dimensions.account_ref = Some("account_other".into());
    aggregate.identity.dimensions.client_key_ref = Some("key_other".into());
    store
        .write_aggregates(aggregates(vec![
            imported,
            contribution("correction", -1, -10, "0"),
        ]))
        .await
        .unwrap();
    let repo = observability_repository(&db.pool);
    let range = ObservabilityRange::new(start(), start() + TimeDelta::days(1)).unwrap();
    use gateway_store::postgres::DiagnosticDimension;
    for (dimension, key) in [
        (DiagnosticDimension::Account, "account_other"),
        (DiagnosticDimension::ApiKey, "key_other"),
    ] {
        let result = repo
            .usage_diagnostics(range, NativeFilter::default(), dimension, 1)
            .await
            .unwrap();
        assert_eq!(result.total_request_count, 12);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].key, key);
        assert_eq!(result.items[0].request_count, 11);
    }
    db.close().await;
}
