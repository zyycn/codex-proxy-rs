//! 用量输入沿用原生记录的数值、时间和资源边界

use crate::model::usage_data::*;
use gateway_core::metering::Decimal;
use serde::Serialize;

pub(super) fn text(value: &str, max: usize) -> UsageDataResult<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(UsageDataError::invalid("文本为空、过长或包含控制字符"));
    }
    Ok(())
}
pub(super) fn bounded(value: &impl Serialize, count: usize) -> UsageDataResult<()> {
    if count == 0
        || count > USAGE_DATA_MAX_TARGETS
        || serde_json::to_vec(value)
            .map_err(|_| UsageDataError::invalid("请求编码失败"))?
            .len()
            > USAGE_DATA_MAX_REQUEST_BYTES
    {
        return Err(UsageDataError::new(
            UsageErrorKind::CapacityExceeded,
            "每次操作接受 1～500 项且不超过 2 MiB",
        ));
    }
    Ok(())
}
pub(super) fn tokens(value: &UsageTokens) -> UsageDataResult<()> {
    if [
        value.input_tokens,
        value.output_tokens,
        value.cached_input_tokens,
        value.cache_write_input_tokens,
        value.reasoning_output_tokens,
        value.image_input_tokens,
        value.image_output_tokens,
        value.total_tokens,
    ]
    .into_iter()
    .flatten()
    .any(|v| v.get() > i64::MAX as u64)
    {
        return Err(UsageDataError::invalid("Token 超出原生存储范围"));
    }
    Ok(())
}
fn cost(value: &UsageCost) -> UsageDataResult<()> {
    if let UsageCost::Known {
        amount, currency, ..
    } = value
    {
        let _ = amount
            .parse::<Decimal>()
            .map_err(|_| UsageDataError::invalid("费用不是有效十进制数"))?;
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(UsageDataError::invalid("费用或币种不符合原生存储范围"));
        }
    }
    Ok(())
}
pub(super) fn change(value: &UsageRecordChange) -> UsageDataResult<()> {
    text(value.id(), 128)?;
    match value {
        UsageRecordChange::Put { data, .. } => {
            text(&data.client_key_ref, 128)?;
            for v in [
                &data.provider,
                &data.account_ref,
                &data.requested_model,
                &data.model,
                &data.service_tier,
            ]
            .into_iter()
            .flatten()
            {
                text(v, 128)?;
            }
            for v in [
                &data.protocol,
                &data.operation,
                &data.endpoint,
                &data.client_transport,
            ] {
                text(v, 256)?;
            }
            if data.outcome == UsageOutcome::Running
                || data
                    .completed_at_ms
                    .is_none_or(|end| end < data.started_at_ms)
            {
                return Err(UsageDataError::invalid("只允许写入已结束的请求明细"));
            }
            for at in [Some(data.started_at_ms), data.completed_at_ms]
                .into_iter()
                .flatten()
            {
                if chrono::DateTime::from_timestamp_millis(at).is_none() {
                    return Err(UsageDataError::invalid("请求时间无效"));
                }
            }
            tokens(&data.tokens)?;
            cost(&data.cost)?;
        }
        UsageRecordChange::Update {
            tokens: new_tokens,
            cost: new_cost,
            ..
        } => {
            if new_tokens.is_none() && new_cost.is_none() {
                return Err(UsageDataError::invalid("修改必须指定 Token 或费用"));
            }
            if let Some(value) = new_tokens {
                tokens(value)?;
            }
            if let Some(value) = new_cost {
                cost(value)?;
            }
        }
        UsageRecordChange::Delete { .. } => {}
    }
    Ok(())
}
pub(super) fn query(value: &ListUsageRecords) -> UsageDataResult<()> {
    if value
        .limit
        .is_some_and(|v| v == 0 || v > USAGE_DATA_MAX_PAGE_SIZE)
        || value.filter.start_ms >= value.filter.end_ms
        || chrono::DateTime::from_timestamp_millis(value.filter.start_ms).is_none()
        || chrono::DateTime::from_timestamp_millis(value.filter.end_ms).is_none()
    {
        return Err(UsageDataError::invalid("查询时间范围或分页大小无效"));
    }
    for v in [
        &value.filter.provider,
        &value.filter.model,
        &value.filter.account_ref,
        &value.filter.client_key_ref,
    ]
    .into_iter()
    .flatten()
    {
        text(v, 128)?;
    }
    if value.cursor.as_ref().is_some_and(|v| v.len() > 4096) {
        return Err(UsageDataError::invalid("游标过长"));
    }
    Ok(())
}

pub(super) fn aggregate(value: &UsageAggregateChange) -> UsageDataResult<()> {
    let identity = match value {
        UsageAggregateChange::Put { aggregate } => &aggregate.identity,
        UsageAggregateChange::Delete { identity } => identity,
    };
    text(&identity.key, 128)?;
    if chrono::DateTime::from_timestamp_millis(identity.bucket_start_ms).is_none() {
        return Err(UsageDataError::invalid("聚合归属时间无效"));
    }
    let d = &identity.dimensions;
    for value in [
        &d.provider,
        &d.model,
        &d.account_ref,
        &d.client_key_ref,
        &d.operation,
        &d.transport,
    ]
    .into_iter()
    .flatten()
    {
        text(value, 128)?;
    }
    if let UsageAggregateChange::Put { aggregate } = value {
        if aggregate.costs.len() > 32 {
            return Err(UsageDataError::invalid("费用币种数量超过限制"));
        }
        for (currency, amount) in &aggregate.costs {
            if currency.len() != 3
                || !currency.bytes().all(|b| b.is_ascii_uppercase())
                || amount.len() > 128
            {
                return Err(UsageDataError::invalid("聚合费用格式无效"));
            }
            amount
                .strip_prefix('-')
                .unwrap_or(amount)
                .parse::<Decimal>()
                .map_err(|_| UsageDataError::invalid("聚合费用不是有效十进制数"))?;
        }
    }
    Ok(())
}
