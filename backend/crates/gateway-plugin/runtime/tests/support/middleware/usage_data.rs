//! 真实插件子进程使用独立 SDK 组合明细、聚合和计价基础能力
use gateway_plugin_sdk::{
    PluginFault,
    call::services::{settings, usage::*},
    client::{ServiceCall, ServiceResponse},
};
pub(super) async fn exercise(call: ServiceCall) -> Result<ServiceResponse, PluginFault> {
    let call = call.into_typed::<settings::ApiKeyExists>()?;
    let data = record_data();
    let write = WriteUsageRecords {
        changes: vec![UsageRecordChange::Put {
            id: "sdk-usage".into(),
            data: Box::new(data.clone()),
        }],
    };
    call.host
        .service::<WriteRecords>(write.clone())
        .await
        .unwrap();
    call.host.service::<WriteRecords>(write).await.unwrap();
    let row = call
        .host
        .service::<GetRecord>(GetUsageRecord {
            id: "sdk-usage".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        row.data.tokens.total_tokens,
        Some(UsageCount(9_007_199_254_740_993))
    );
    let filter = UsageRecordFilter {
        start_ms: 1780272000000,
        end_ms: 1780272002000,
        provider: None,
        model: None,
        account_ref: None,
        client_key_ref: None,
    };
    assert_eq!(
        call.host
            .service::<ListRecords>(ListUsageRecords {
                filter: filter.clone(),
                cursor: None,
                limit: Some(10)
            })
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let scan_request = ScanUsageRecords {
        filter: UsageReadFilter {
            records: filter.clone(),
            routing_group_ref: None,
        },
        batch_size: Some(1),
    };
    let mut reader = call.host.scan_usage_records(scan_request.clone()).await?;
    let first = reader.next_batch().await?.unwrap();
    let UsageReadItem::Record(fact) = &first.items[0] else {
        panic!("record expected");
    };
    assert_eq!(fact.id, "sdk-usage");
    assert_eq!(fact.data.client_key_ref, "sdk-key");
    assert!(fact.usage_counted);
    assert!(!first.complete);
    assert!(reader.next_batch().await?.unwrap().complete);
    assert!(reader.next_batch().await?.is_none());
    let mut reader = call.host.scan_usage_records(scan_request).await?;
    reader.close().await?;
    assert!(reader.next_batch().await?.is_none());
    let quote=call.host.service::<EvaluatePricing>(EvaluateUsagePricing{items:vec![UsagePricingInput{id:"quote".into(),provider:"openai".into(),model:"gpt-6-sol".into(),tokens:data.tokens,service_tier:None,price:UsagePriceSelection::Override{price:Box::new(serde_json::from_value(serde_json::json!({"multiplierBps":10000,"bands":{"standard":{"input":"2","output":"10","cacheRead":"0.5","cacheWrite":"2"}}})).unwrap())}}]}).await.unwrap();
    let UsagePricingResult::Estimated { cost, .. } = quote.items[0].clone() else {
        panic!("expected quote")
    };
    call.host
        .service::<WriteRecords>(WriteUsageRecords {
            changes: vec![UsageRecordChange::Update {
                id: "sdk-usage".into(),
                tokens: None,
                cost: Some(cost),
            }],
        })
        .await
        .unwrap();
    let identity = UsageAggregateKey {
        bucket_start_ms: 1780272000000,
        dimensions: UsageDimensions::default(),
        key: "sdk-daily".into(),
    };
    call.host
        .service::<WriteAggregates>(WriteUsageAggregates {
            changes: vec![UsageAggregateChange::Put {
                aggregate: UsageAggregate {
                    identity: identity.clone(),
                    counts: std::collections::BTreeMap::from([(
                        UsageMetric::RequestCount,
                        UsageDelta(100),
                    )]),
                    costs: Default::default(),
                },
            }],
        })
        .await
        .unwrap();
    let mut statistics = call
        .host
        .query_usage_statistics(QueryUsageStatistics {
            filter: UsageReadFilter {
                records: filter.clone(),
                routing_group_ref: None,
            },
            group_by: UsageGrouping::Key,
            batch_size: Some(1),
        })
        .await?;
    let total = statistics.next_batch().await?.unwrap();
    let UsageReadItem::Statistics(total) = &total.items[0] else {
        panic!("statistics expected");
    };
    assert!(total.group.is_none());
    assert_eq!(
        total.metrics.values[&UsageMetric::RequestCount],
        UsageCount(100)
    );
    let group = statistics.next_batch().await?.unwrap();
    let UsageReadItem::Statistics(group) = &group.items[0] else {
        panic!("statistics expected");
    };
    assert!(group.group.as_ref().unwrap().unassigned_contribution);
    assert!(statistics.next_batch().await?.unwrap().complete);
    assert!(statistics.next_batch().await?.is_none());
    assert_eq!(
        call.host
            .service::<ListAggregates>(ListUsageAggregates {
                filter,
                cursor: None,
                limit: None
            })
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    call.host
        .service::<WriteAggregates>(WriteUsageAggregates {
            changes: vec![UsageAggregateChange::Delete { identity }],
        })
        .await
        .unwrap();
    call.host
        .service::<WriteRecords>(WriteUsageRecords {
            changes: vec![
                UsageRecordChange::Put {
                    id: "sdk-delete".into(),
                    data: Box::new(row.data),
                },
                UsageRecordChange::Delete {
                    id: "sdk-delete".into(),
                },
            ],
        })
        .await
        .unwrap();
    ServiceResponse::from_result::<settings::ApiKeyExists>(Ok(true))
}

fn record_data() -> UsageRecordData {
    UsageRecordData {
        client_key_ref: "sdk-key".into(),
        provider: Some("openai".into()),
        account_ref: None,
        requested_model: Some("gpt-6-sol".into()),
        model: Some("gpt-6-sol".into()),
        protocol: "openai".into(),
        operation: "responses".into(),
        endpoint: "/v1/responses".into(),
        client_transport: "http_sse".into(),
        service_tier: None,
        started_at_ms: 1780272000000,
        completed_at_ms: Some(1780272001000),
        outcome: UsageOutcome::Succeeded,
        tokens: UsageTokens {
            input_tokens: Some(UsageCount(100)),
            output_tokens: Some(UsageCount(20)),
            cached_input_tokens: Some(UsageCount(0)),
            cache_write_input_tokens: Some(UsageCount(0)),
            image_input_tokens: Some(UsageCount(0)),
            image_output_tokens: Some(UsageCount(0)),
            total_tokens: Some(UsageCount(9_007_199_254_740_993)),
            ..Default::default()
        },
        cost: UsageCost::Unavailable,
    }
}

pub(super) async fn hold(
    call: ServiceCall,
    marker: std::path::PathBuf,
) -> Result<ServiceResponse, PluginFault> {
    let call = call.into_typed::<settings::ApiKeyExists>()?;
    call.host
        .service::<WriteRecords>(WriteUsageRecords {
            changes: vec![UsageRecordChange::Put {
                id: "held-usage".into(),
                data: Box::new(record_data()),
            }],
        })
        .await
        .unwrap();
    let mut reader = call
        .host
        .scan_usage_records(ScanUsageRecords {
            filter: UsageReadFilter {
                records: UsageRecordFilter {
                    start_ms: 1780272000000,
                    end_ms: 1780272002000,
                    provider: None,
                    model: None,
                    account_ref: None,
                    client_key_ref: None,
                },
                routing_group_ref: None,
            },
            batch_size: Some(1),
        })
        .await?;
    assert!(!reader.next_batch().await?.unwrap().complete);
    std::fs::write(marker.with_extension("opened"), b"opened").unwrap();
    while !marker.with_extension("release").exists() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // 故意不显式关闭，验证宿主在父调用终结时回收快照
    ServiceResponse::from_result::<settings::ApiKeyExists>(Ok(true))
}
