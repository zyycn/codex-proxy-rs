//! 原生用量能力的类型化操作标识

use super::*;
use crate::call::services::Operation;

pub struct ListRecords;
impl Operation for ListRecords {
    const NAME: &'static str = "usage.records.list";
    type Input = ListUsageRecords;
    type Output = UsageRecordPage;
}

pub struct GetRecord;
impl Operation for GetRecord {
    const NAME: &'static str = "usage.records.get";
    type Input = GetUsageRecord;
    type Output = UsageRecord;
}

pub struct WriteRecords;
impl Operation for WriteRecords {
    const NAME: &'static str = "usage.records.write";
    type Input = WriteUsageRecords;
    type Output = UsageWriteResult;
}

pub struct EvaluatePricing;
impl Operation for EvaluatePricing {
    const NAME: &'static str = "pricing.evaluate";
    type Input = EvaluateUsagePricing;
    type Output = UsagePricingEvaluation;
}

pub struct ListAggregates;
impl Operation for ListAggregates {
    const NAME: &'static str = "usage.aggregates.list";
    type Input = ListUsageAggregates;
    type Output = UsageAggregatePage;
}
pub struct WriteAggregates;
impl Operation for WriteAggregates {
    const NAME: &'static str = "usage.aggregates.write";
    type Input = WriteUsageAggregates;
    type Output = UsageAggregateWriteResult;
}
