//! 用量读取的完整分批、统计覆盖、一致快照与取消预算验证

use super::*;
use gateway_core::lifecycle::CancellationToken;
use std::{collections::BTreeSet, time::Instant};

fn read_filter() -> UsageReadFilter {
    UsageReadFilter {
        records: filter(),
        routing_group_ref: None,
    }
}

fn scan(batch_size: u16) -> UsageReadQuery {
    UsageReadQuery::Records(ScanUsageRecords {
        filter: read_filter(),
        batch_size: Some(batch_size),
    })
}

async fn statistics(
    store: &PgUsageDataStore,
    filter: UsageReadFilter,
    group_by: UsageGrouping,
) -> Vec<UsageStatisticsRow> {
    let mut reader = store
        .open_read(
            UsageReadQuery::Statistics(QueryUsageStatistics {
                filter,
                group_by,
                batch_size: Some(17),
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut rows = Vec::new();
    let mut observation = None;
    loop {
        let batch = reader.next_batch().await.unwrap();
        assert!(serde_json::to_vec(&batch).unwrap().len() <= USAGE_READ_MAX_BATCH_BYTES);
        if let Some(observation) = &observation {
            assert_eq!(observation, &batch.observation);
        }
        observation = Some(batch.observation.clone());
        for item in batch.items {
            let UsageReadItem::Statistics(row) = item else {
                panic!("statistics row expected");
            };
            rows.push(*row);
        }
        if batch.complete {
            break;
        }
    }
    assert_eq!(
        observation.unwrap().total_groups.unwrap().get(),
        (rows.len() - 1) as u64
    );
    assert!(rows[0].group.is_none());
    rows
}

#[tokio::test]
async fn scan_delivers_ten_thousand_records_from_one_snapshot() {
    let Some(db) = TestDatabase::create("usage_scan").await else {
        return;
    };
    let store = PgUsageDataStore::new(
        db.pool.clone(),
        ObservabilityQueryBudget::try_new(2, Duration::from_secs(5), Duration::from_secs(60))
            .unwrap(),
    );
    for base in (0..10_000).step_by(500) {
        store
            .write_records(records(
                (base..base + 500)
                    .map(|index| put(&format!("request_{index:05}"), 10))
                    .collect(),
            ))
            .await
            .unwrap();
    }
    sqlx::query("update model_requests set request_observation_json=jsonb_set(request_observation_json,'{request}',(request_observation_json->'request') || '{\"userAgent\":\"usage-test\",\"reasoningEffort\":\"high\",\"subagentKind\":\"worker\"}'::jsonb)").execute(&db.pool).await.unwrap();
    let baseline_started = Instant::now();
    let mut cursor = None;
    let mut baseline_batches = 0;
    loop {
        let page = store
            .list_records(ListUsageRecords {
                filter: filter(),
                cursor,
                limit: Some(200),
            })
            .await
            .unwrap();
        baseline_batches += 1;
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let baseline_elapsed = baseline_started.elapsed();
    let started = Instant::now();
    let mut reader = store
        .open_read(scan(1024), CancellationToken::new())
        .await
        .unwrap();
    let first = reader.next_batch().await.unwrap();
    let observation = first.observation.clone();
    store
        .write_records(records(vec![
            UsageRecordChange::Delete {
                id: "request_09999".into(),
            },
            put("request_new", 99),
            put("request_09000", 99),
        ]))
        .await
        .unwrap();
    let mut batch = first;
    let mut ids = BTreeSet::new();
    let mut batches = 0;
    loop {
        assert_eq!(batch.observation, observation);
        batches += 1;
        for item in batch.items {
            let UsageReadItem::Record(record) = item else {
                panic!("record expected");
            };
            assert_eq!(record.data.tokens.total_tokens, Some(UsageCount(10)));
            assert_eq!(record.user_agent.as_deref(), Some("usage-test"));
            assert_eq!(record.reasoning_effort.as_deref(), Some("high"));
            assert_eq!(record.subagent_kind.as_deref(), Some("worker"));
            assert!(record.request_counted && record.usage_counted);
            assert!(ids.insert(record.id));
        }
        if batch.complete {
            break;
        }
        batch = reader.next_batch().await.unwrap();
    }
    assert_eq!(ids.len(), 10_000);
    assert!(ids.contains("request_09999") && !ids.contains("request_new"));
    eprintln!(
        "usage scan rows=10000 baseline_batches={baseline_batches} baseline_ms={} scan_batches={batches} scan_ms={}",
        baseline_elapsed.as_millis(),
        started.elapsed().as_millis()
    );
    let current = store
        .get_record(GetUsageRecord {
            id: "request_09000".into(),
        })
        .await
        .unwrap();
    assert_eq!(current.data.tokens.total_tokens, Some(UsageCount(99)));
    reader.close().await;
    db.close().await;
}

#[tokio::test]
async fn statistics_preserve_all_groups_unknowns_contributions_and_overlapping_routes() {
    let Some(db) = TestDatabase::create("usage_statistics").await else {
        return;
    };
    let store = PgUsageDataStore::new(db.pool.clone(), observability_query_budget());
    let mut changes = Vec::new();
    for index in 0..137 {
        let mut value = data(10);
        value.client_key_ref = format!("key_{index}");
        value.requested_model = Some("alias".into());
        value.model = Some(format!("model_{}", index % 2));
        value.started_at_ms += 1000;
        value.completed_at_ms = Some(value.started_at_ms + 1000);
        value.tokens.cached_input_tokens = Some(UsageCount(0));
        value.cost = UsageCost::Known {
            source: UsageCostSource::Calculated,
            amount: "1".into(),
            currency: if index % 2 == 0 { "USD" } else { "EUR" }.into(),
        };
        changes.push(UsageRecordChange::Put {
            id: format!("row_{index}"),
            data: Box::new(value),
        });
    }
    let mut prewarm = data(999);
    prewarm.client_key_ref = "key_0".into();
    prewarm.model = Some("model_0".into());
    prewarm.started_at_ms += 1000;
    prewarm.completed_at_ms = Some(prewarm.started_at_ms + 1000);
    changes.push(UsageRecordChange::Put {
        id: "prewarm".into(),
        data: Box::new(prewarm),
    });
    store.write_records(records(changes)).await.unwrap();
    sqlx::query("update model_requests set request_kind='prewarm' where id='prewarm'")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update model_requests set request_observation_json=jsonb_set(request_observation_json,'{routing}','{\"scope\":\"groups\",\"groupRefs\":[\"g1\",\"g2\"],\"groupNamesSnapshot\":[\"one\",\"two\"]}'::jsonb) where id='row_0'").execute(&db.pool).await.unwrap();
    let UsageAggregateChange::Put { mut aggregate } = contribution("known", 2, 20, "0.5") else {
        unreachable!();
    };
    aggregate.identity.bucket_start_ms += 1000;
    aggregate.identity.dimensions.client_key_ref = Some("key_0".into());
    aggregate.identity.dimensions.model = Some("model_0".into());
    let mut unknown = aggregate.clone();
    unknown.identity.key = "unknown".into();
    unknown.identity.dimensions = UsageDimensions::default();
    unknown.counts = BTreeMap::from([(UsageMetric::RequestCount, UsageDelta(3))]);
    unknown.costs.clear();
    store
        .write_aggregates(aggregates(vec![
            UsageAggregateChange::Put { aggregate },
            UsageAggregateChange::Put { aggregate: unknown },
        ]))
        .await
        .unwrap();
    drain_analytics(&db.pool).await;
    let mut filter = read_filter();
    filter.records.start_ms += 500;
    filter.records.end_ms -= 500;
    let rows = statistics(&store, filter.clone(), UsageGrouping::KeyModel).await;
    let projected = statistics(&store, read_filter(), UsageGrouping::KeyModel).await;
    assert_eq!(rows.len(), projected.len());
    for (edge, full) in rows.iter().zip(&projected) {
        assert_eq!(edge, full);
    }
    assert_eq!(
        statistics(&store, read_filter(), UsageGrouping::Key)
            .await
            .len(),
        139
    );
    assert_eq!(
        statistics(&store, read_filter(), UsageGrouping::Model)
            .await
            .len(),
        4
    );
    let mut selected = read_filter();
    selected.records.client_key_ref = Some("key_0".into());
    let selected_rows = statistics(&store, selected, UsageGrouping::Model).await;
    assert_eq!(
        selected_rows[0].metrics.values[&UsageMetric::RequestCount].get(),
        4
    );
    assert_eq!(
        selected_rows[0].metrics.values[&UsageMetric::TotalTokens].get(),
        30
    );
    assert_eq!(rows.len(), 139);
    let total = &rows[0].metrics;
    assert_eq!(total.values[&UsageMetric::RequestCount].get(), 143);
    assert_eq!(total.values[&UsageMetric::CompletedRequestCount].get(), 139);
    assert_eq!(total.values[&UsageMetric::TotalTokens].get(), 1390);
    assert_eq!(
        total.native_token_samples[&UsageMetric::InputTokens].get(),
        137
    );
    assert_eq!(
        total.contribution_token_samples[&UsageMetric::TotalTokens].get(),
        1
    );
    assert!(!total.values.contains_key(&UsageMetric::OutputTokens));
    assert_eq!(total.values[&UsageMetric::CachedInputTokens].get(), 0);
    assert_eq!(total.cost_coverage.calculated.get(), 137);
    assert_eq!(
        total
            .costs
            .iter()
            .find(|cost| cost.currency == "USD")
            .unwrap()
            .amount,
        "69.5"
    );
    assert_eq!(
        total
            .costs
            .iter()
            .find(|cost| cost.currency == "EUR")
            .unwrap()
            .amount,
        "68"
    );
    let routes = statistics(&store, filter.clone(), UsageGrouping::RoutingGroup).await;
    assert_eq!(routes[0].metrics, *total);
    for group in ["g1", "g2"] {
        let row = routes
            .iter()
            .find(|row| {
                row.group
                    .as_ref()
                    .is_some_and(|key| key.routing_group_ref.as_deref() == Some(group))
            })
            .unwrap();
        assert_eq!(row.metrics.values[&UsageMetric::RequestCount].get(), 1);
    }
    filter.routing_group_ref = Some("g1".into());
    let scoped = statistics(&store, filter.clone(), UsageGrouping::Key).await;
    assert_eq!(
        scoped[0].metrics.values[&UsageMetric::RequestCount].get(),
        1
    );
    assert_eq!(scoped[0].metrics.contribution_count.get(), 0);
    filter.routing_group_ref = None;
    filter.records.model = Some("alias".into());
    let empty = statistics(&store, filter, UsageGrouping::Model).await;
    assert_eq!(empty.len(), 1);
    assert_eq!(empty[0].metrics.values[&UsageMetric::RequestCount].get(), 0);
    let UsageAggregateChange::Put { mut aggregate } = contribution("negative-group", -100, 0, "0")
    else {
        unreachable!();
    };
    aggregate.identity.dimensions.client_key_ref = Some("zzz".into());
    aggregate.identity.dimensions.model = Some("model_0".into());
    aggregate.counts = BTreeMap::from([(UsageMetric::RequestCount, UsageDelta(-100))]);
    aggregate.costs.clear();
    store
        .write_aggregates(aggregates(vec![UsageAggregateChange::Put { aggregate }]))
        .await
        .unwrap();
    let mut reader = store
        .open_read(
            UsageReadQuery::Statistics(QueryUsageStatistics {
                filter: read_filter(),
                group_by: UsageGrouping::KeyModel,
                batch_size: Some(1),
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!reader.next_batch().await.unwrap().complete);
    let error = loop {
        match reader.next_batch().await {
            Ok(batch) => assert!(!batch.complete, "negative group must fail the read"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.kind, UsageErrorKind::InvalidInput);
    reader.close().await;

    // 合法贡献各自有界，合并后的单个统计行仍须服从交付字节上限
    let currencies = (b'A'..=b'Z')
        .flat_map(|first| {
            (b'A'..=b'Z').flat_map(move |second| {
                (b'A'..=b'Z')
                    .map(move |third| String::from_utf8(vec![first, second, third]).unwrap())
            })
        })
        .collect::<Vec<_>>();
    let native_changes = currencies
        .iter()
        .take(800)
        .enumerate()
        .map(|(index, currency)| {
            let mut value = data(1);
            value.model = Some("large-costs".into());
            value.cost = UsageCost::Known {
                source: UsageCostSource::Calculated,
                amount: "0.1234567891".into(),
                currency: currency.clone(),
            };
            UsageRecordChange::Put {
                id: format!("cost-{index}"),
                data: Box::new(value),
            }
        })
        .collect::<Vec<_>>();
    for changes in native_changes.chunks(500) {
        store
            .write_records(records(changes.to_vec()))
            .await
            .unwrap();
    }
    drain_analytics(&db.pool).await;
    let changes = currencies
        .chunks(32)
        .enumerate()
        .map(|(index, currencies)| UsageAggregateChange::Put {
            aggregate: UsageAggregate {
                identity: UsageAggregateKey {
                    bucket_start_ms: read_filter().records.start_ms,
                    dimensions: UsageDimensions {
                        model: Some("large-costs".into()),
                        ..Default::default()
                    },
                    key: format!("costs-{index}"),
                },
                counts: BTreeMap::new(),
                costs: currencies
                    .iter()
                    .map(|currency| (currency.clone(), "1234567890.1234567891".into()))
                    .collect(),
            },
        })
        .collect::<Vec<_>>();
    for changes in changes.chunks(500) {
        store
            .write_aggregates(aggregates(changes.to_vec()))
            .await
            .unwrap();
    }
    let mut large_filter = read_filter();
    large_filter.records.model = Some("large-costs".into());
    let mut reader = store
        .open_read(
            UsageReadQuery::Statistics(QueryUsageStatistics {
                filter: large_filter,
                group_by: UsageGrouping::Model,
                batch_size: Some(1),
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        reader.next_batch().await.unwrap_err().kind,
        UsageErrorKind::CapacityExceeded
    );
    reader.close().await;
    db.close().await;
}

#[tokio::test]
async fn byte_boundaries_timeout_and_cancellation_release_readers() {
    let Some(db) = TestDatabase::create("usage_read_budget").await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(1, Duration::from_secs(2), Duration::from_millis(500))
            .unwrap();
    let store = PgUsageDataStore::new(db.pool.clone(), budget);
    store
        .write_records(records(
            (0..8)
                .map(|index| put(&format!("large_{index}"), 1))
                .collect(),
        ))
        .await
        .unwrap();
    sqlx::query("update model_requests set request_observation_json=jsonb_set(request_observation_json,'{request,userAgent}',to_jsonb(repeat('x',700000)))").execute(&db.pool).await.unwrap();
    let mut reader = store
        .open_read(scan(1024), CancellationToken::new())
        .await
        .unwrap();
    let first = reader.next_batch().await.unwrap();
    assert!(!first.complete && first.items.len() < 8);
    assert!(serde_json::to_vec(&first).unwrap().len() <= USAGE_READ_MAX_BATCH_BYTES);
    reader.close().await;
    let cancellation = CancellationToken::new();
    let mut reader = store
        .open_read(scan(1), cancellation.clone())
        .await
        .unwrap();
    assert_eq!(reader.next_batch().await.unwrap().items.len(), 1);
    cancellation.cancel();
    assert_eq!(
        reader.next_batch().await.unwrap_err().kind,
        UsageErrorKind::Cancelled
    );
    let mut reader = store
        .open_read(scan(1), CancellationToken::new())
        .await
        .unwrap();
    reader.next_batch().await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        reader.next_batch().await.unwrap_err().kind,
        UsageErrorKind::Timeout
    );
    let mut after = store
        .open_read(scan(1), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(after.next_batch().await.unwrap().items.len(), 1);
    after.close().await;
    db.close().await;
}
