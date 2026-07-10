//! 管理端使用记录服务。

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::{
    infra::json::{NumberedPage, Page},
    telemetry::{
        billing,
        usage::store::{
            PgUsageRecordStore, UsageRecordAccountUsage, UsageRecordBreakdown,
            UsageRecordEndpointSource, UsageRecordFilter, UsageRecordModelSource,
            UsageRecordSummary, UsageRecordTrendPoint,
        },
        usage::types::UsageRecord,
    },
};

/// 管理端使用记录查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageQueryFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 调用方客户端 API key ID。
    pub client_api_key_id: Option<String>,
    /// 上游 provider。
    pub provider: Option<String>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
    /// 起始时间。
    pub start_time: Option<DateTime<Utc>>,
    /// 结束时间。
    pub end_time: Option<DateTime<Utc>>,
}

impl From<UsageQueryFilter> for UsageRecordFilter {
    fn from(filter: UsageQueryFilter) -> Self {
        Self {
            kind: filter.kind,
            client_api_key_id: filter.client_api_key_id,
            provider: filter.provider,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            transport: filter.transport,
            attempt_index: filter.attempt_index,
            response_id: filter.response_id,
            upstream_request_id: filter.upstream_request_id,
            search: filter.search,
            start_time: filter.start_time,
            end_time: filter.end_time,
        }
    }
}

/// 清空使用记录结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearUsageRecords {
    /// 清理数量。
    pub cleared: u64,
}

/// 管理端使用记录费用明细。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UsageRecordCostDetails {
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub total_cost: f64,
    pub original_cost: f64,
    pub input_price_per_mtoken: f64,
    pub output_price_per_mtoken: f64,
    pub cache_read_price_per_mtoken: f64,
    pub service_tier: Option<String>,
    pub service_tier_display: String,
    pub multiplier: f64,
}

/// 管理端使用记录错误。
#[derive(Debug, Error)]
pub enum UsageQueryError {
    /// 列表失败。
    #[error("failed to list usage records")]
    List,
    /// 读取失败。
    #[error("failed to get usage record")]
    Get,
    /// 清空失败。
    #[error("failed to clear usage records")]
    Clear,
    /// 账号关联信息读取失败。
    #[error("failed to load usage record accounts")]
    Accounts,
}

/// 管理端使用记录服务。
#[derive(Clone)]
pub struct UsageQueryService {
    store: PgUsageRecordStore,
}

impl UsageQueryService {
    /// 构造管理端使用记录服务。
    pub fn new(store: PgUsageRecordStore) -> Self {
        Self { store }
    }

    /// 分页查询日志。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
        filter: UsageQueryFilter,
    ) -> Result<Page<UsageRecord>, UsageQueryError> {
        self.store
            .list(filter.into(), cursor, limit)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按页码查询日志。
    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        filter: UsageQueryFilter,
    ) -> Result<NumberedPage<UsageRecord>, UsageQueryError> {
        self.store
            .list_page(filter.into(), page, page_size)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按 ID 读取日志。
    pub async fn get(&self, id: &str) -> Result<Option<UsageRecord>, UsageQueryError> {
        self.store.get(id).await.map_err(|_| UsageQueryError::Get)
    }

    /// 读取使用记录关联账号的邮箱映射。
    pub async fn account_email_map(
        &self,
        items: &[UsageRecord],
    ) -> Result<HashMap<String, String>, UsageQueryError> {
        self.store
            .account_email_map(items)
            .await
            .map_err(|_| UsageQueryError::Accounts)
    }

    /// 汇总使用记录。
    pub async fn summary(
        &self,
        filter: UsageQueryFilter,
    ) -> Result<UsageRecordSummary, UsageQueryError> {
        self.store
            .summary(filter.into())
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按账号聚合使用记录。
    pub async fn account_usage(
        &self,
        filter: UsageQueryFilter,
        limit: u32,
    ) -> Result<Vec<UsageRecordAccountUsage>, UsageQueryError> {
        self.store
            .account_usage(filter.into(), limit)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按模型来源聚合使用记录分布。
    pub async fn model_distribution(
        &self,
        filter: UsageQueryFilter,
        source: UsageRecordModelSource,
    ) -> Result<Vec<UsageRecordBreakdown>, UsageQueryError> {
        self.store
            .model_distribution(filter.into(), source)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按端点来源聚合使用记录分布。
    pub async fn endpoint_distribution(
        &self,
        filter: UsageQueryFilter,
        source: UsageRecordEndpointSource,
    ) -> Result<Vec<UsageRecordBreakdown>, UsageQueryError> {
        self.store
            .endpoint_distribution(filter.into(), source)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 聚合使用记录趋势。
    pub async fn trend(
        &self,
        filter: UsageQueryFilter,
    ) -> Result<Vec<UsageRecordTrendPoint>, UsageQueryError> {
        self.store
            .trend(filter.into())
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 清空日志。
    pub async fn clear(&self) -> Result<ClearUsageRecords, UsageQueryError> {
        self.store
            .clear()
            .await
            .map(|cleared| ClearUsageRecords { cleared })
            .map_err(|_| UsageQueryError::Clear)
    }
}

pub(crate) fn usage_record_cost_details(
    record: &UsageRecord,
    upstream_model: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> Option<UsageRecordCostDetails> {
    if input_tokens == 0 && output_tokens == 0 && cached_tokens == 0 {
        return None;
    }

    let model = upstream_model
        .or(record.upstream_model.as_deref())
        .or(Some(record.model.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let service_tier = record.service_tier.as_deref();
    let breakdown = billing::calculate_cost_breakdown(
        input_tokens,
        output_tokens,
        cached_tokens,
        model,
        service_tier,
    );
    let original_cost = breakdown.input_cost + breakdown.output_cost + breakdown.cache_read_cost;

    Some(UsageRecordCostDetails {
        input_cost: breakdown.input_cost,
        output_cost: breakdown.output_cost,
        cache_read_cost: breakdown.cache_read_cost,
        total_cost: breakdown.total_cost,
        original_cost,
        input_price_per_mtoken: breakdown.input_price_per_mtoken,
        output_price_per_mtoken: breakdown.output_price_per_mtoken,
        cache_read_price_per_mtoken: breakdown.cache_read_price_per_mtoken,
        service_tier: breakdown.service_tier.clone(),
        service_tier_display: breakdown
            .service_tier
            .as_deref()
            .map(format_service_tier)
            .unwrap_or_else(|| "Default".to_string()),
        multiplier: breakdown.tier_multiplier,
    })
}

fn format_service_tier(value: &str) -> String {
    match value {
        "priority" | "fast" => "Fast".to_string(),
        "flex" => "Flex".to_string(),
        "default" => "Default".to_string(),
        other => other.to_string(),
    }
}
