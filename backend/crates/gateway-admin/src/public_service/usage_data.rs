//! 原生用量能力的公开服务登记

use crate::{
    UsageDataService,
    model::{AdminError, usage_data::*},
};
use std::sync::Arc;

pub(super) fn register(
    registry: &mut super::Registry,
    service: &Arc<dyn UsageDataService>,
) -> Result<(), AdminError> {
    macro_rules! read {
        ($name:literal, $method:ident, $input:ty) => {{
            let service = service.clone();
            registry.register_usage($name, move |request: $input| {
                let service = service.clone();
                Box::pin(async move { service.$method(request).await })
            })?;
        }};
    }
    read!("usage.records.get", get_record, GetUsageRecord);
    read!("usage.records.list", list_records, ListUsageRecords);
    read!(
        "usage.aggregates.list",
        list_aggregates,
        ListUsageAggregates
    );
    read!("pricing.evaluate", evaluate_pricing, EvaluateUsagePricing);
    let aggregates = service.clone();
    registry.register_usage(
        "usage.aggregates.write",
        move |request: WriteUsageAggregates| {
            let service = aggregates.clone();
            Box::pin(async move {
                let actor = super::actor::current_actor()?;
                service.write_aggregates(&actor, request).await
            })
        },
    )?;
    let service = service.clone();
    registry.register_usage("usage.records.write", move |request: WriteUsageRecords| {
        let service = service.clone();
        Box::pin(async move {
            let actor = super::actor::current_actor()?;
            service.write_records(&actor, request).await
        })
    })?;
    Ok(())
}

pub(super) fn encode_error(
    error: UsageDataError,
) -> gateway_core::engine::middleware::service::Error {
    gateway_core::engine::middleware::service::Error {
        kind: serde_json::to_value(error.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unavailable".into()),
        message: error.message.clone(),
        details: serde_json::to_value(error).ok(),
    }
}
