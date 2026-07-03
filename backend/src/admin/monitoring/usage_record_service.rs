//! 管理端使用记录服务。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::{
    admin::monitoring::{
        billing,
        usage_record_model::metadata_service_tier,
        usage_record_model::{UsageRecord, UsageRecordLevel},
        usage_record_store::{
            SqliteUsageRecordStore, UsageRecordBreakdown, UsageRecordEndpointSource,
            UsageRecordFilter, UsageRecordModelSource, UsageRecordSummary, UsageRecordTrendPoint,
        },
    },
    infra::json::{NumberedPage, Page},
};

/// 管理端使用记录查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminUsageRecordFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<UsageRecordLevel>,
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
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
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

impl From<AdminUsageRecordFilter> for UsageRecordFilter {
    fn from(filter: AdminUsageRecordFilter) -> Self {
        Self {
            kind: filter.kind,
            level: filter.level,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            transport: filter.transport,
            attempt_index: filter.attempt_index,
            upstream_status_code: filter.upstream_status_code,
            failure_class: filter.failure_class,
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
pub struct AdminClearUsageRecords {
    /// 清理数量。
    pub cleared: u64,
}

/// 管理端使用记录费用明细。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AdminUsageRecordCostDetails {
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub total_cost: f64,
    pub billed_cost: f64,
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
pub enum AdminUsageRecordError {
    /// 列表失败。
    #[error("failed to list usage records")]
    List,
    /// 读取失败。
    #[error("failed to get usage record")]
    Get,
    /// 清空失败。
    #[error("failed to clear usage records")]
    Clear,
    /// 写入失败。
    #[error("failed to append usage record")]
    Append,
    /// 裁剪失败。
    #[error("failed to trim usage records")]
    Trim,
    /// 账号关联信息读取失败。
    #[error("failed to load usage record accounts")]
    Accounts,
}

/// 管理端使用记录服务。
#[derive(Clone)]
pub struct AdminUsageRecordService {
    store: SqliteUsageRecordStore,
    settings: Arc<tokio::sync::RwLock<AdminUsageRecordSettings>>,
}

#[derive(Debug, Clone, Copy)]
struct AdminUsageRecordSettings {
    enabled: bool,
    capacity: u32,
    capture_body: bool,
}

impl AdminUsageRecordService {
    /// 构造管理端使用记录服务。
    pub fn new(
        store: SqliteUsageRecordStore,
        enabled: bool,
        capacity: u32,
        capture_body: bool,
    ) -> Self {
        Self {
            store,
            settings: Arc::new(tokio::sync::RwLock::new(AdminUsageRecordSettings {
                enabled,
                capacity,
                capture_body,
            })),
        }
    }

    /// 分页查询日志。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
        filter: AdminUsageRecordFilter,
    ) -> Result<Page<UsageRecord>, AdminUsageRecordError> {
        self.store
            .list(filter.into(), cursor, limit)
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 按页码查询日志。
    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        filter: AdminUsageRecordFilter,
    ) -> Result<NumberedPage<UsageRecord>, AdminUsageRecordError> {
        self.store
            .list_page(filter.into(), page, page_size)
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 按 ID 读取日志。
    pub async fn get(&self, id: &str) -> Result<Option<UsageRecord>, AdminUsageRecordError> {
        self.store
            .get(id)
            .await
            .map_err(|_| AdminUsageRecordError::Get)
    }

    /// 读取使用记录关联账号的邮箱映射。
    pub async fn account_email_map(
        &self,
        items: &[UsageRecord],
    ) -> Result<HashMap<String, String>, AdminUsageRecordError> {
        self.store
            .account_email_map(items)
            .await
            .map_err(|_| AdminUsageRecordError::Accounts)
    }

    /// 汇总使用记录。
    pub async fn summary(
        &self,
        filter: AdminUsageRecordFilter,
    ) -> Result<UsageRecordSummary, AdminUsageRecordError> {
        self.store
            .summary(filter.into())
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 按模型来源聚合使用记录分布。
    pub async fn model_distribution(
        &self,
        filter: AdminUsageRecordFilter,
        source: UsageRecordModelSource,
    ) -> Result<Vec<UsageRecordBreakdown>, AdminUsageRecordError> {
        self.store
            .model_distribution(filter.into(), source)
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 按端点来源聚合使用记录分布。
    pub async fn endpoint_distribution(
        &self,
        filter: AdminUsageRecordFilter,
        source: UsageRecordEndpointSource,
    ) -> Result<Vec<UsageRecordBreakdown>, AdminUsageRecordError> {
        self.store
            .endpoint_distribution(filter.into(), source)
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 聚合 Token 趋势。
    pub async fn token_trend(
        &self,
        filter: AdminUsageRecordFilter,
    ) -> Result<Vec<UsageRecordTrendPoint>, AdminUsageRecordError> {
        self.store
            .trend(filter.into())
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 聚合延迟趋势。
    pub async fn latency_trend(
        &self,
        filter: AdminUsageRecordFilter,
    ) -> Result<Vec<UsageRecordTrendPoint>, AdminUsageRecordError> {
        self.store
            .trend(filter.into())
            .await
            .map_err(|_| AdminUsageRecordError::List)
    }

    /// 清空日志。
    pub async fn clear(&self) -> Result<AdminClearUsageRecords, AdminUsageRecordError> {
        self.store
            .clear()
            .await
            .map(|cleared| AdminClearUsageRecords { cleared })
            .map_err(|_| AdminUsageRecordError::Clear)
    }

    /// 记录使用记录。
    pub async fn record(&self, event: UsageRecord) -> Result<(), AdminUsageRecordError> {
        let settings = *self.settings.read().await;
        if !settings.enabled && event.level != UsageRecordLevel::Error {
            return Ok(());
        }
        self.append_with_settings(event, settings).await
    }

    async fn append_with_settings(
        &self,
        mut event: UsageRecord,
        settings: AdminUsageRecordSettings,
    ) -> Result<(), AdminUsageRecordError> {
        apply_capture_body_policy(&mut event, settings.capture_body);
        self.store
            .append(&event)
            .await
            .map_err(|_| AdminUsageRecordError::Append)?;
        self.store
            .trim_to_capacity(settings.capacity)
            .await
            .map_err(|_| AdminUsageRecordError::Trim)?;
        Ok(())
    }
}

fn apply_capture_body_policy(event: &mut UsageRecord, capture_body: bool) {
    if capture_body {
        return;
    }
    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ] {
        metadata.remove(key);
    }
}

pub(crate) fn usage_record_cost_details(
    record: &UsageRecord,
    upstream_model: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> Option<AdminUsageRecordCostDetails> {
    if input_tokens == 0 && output_tokens == 0 && cached_tokens == 0 {
        return None;
    }

    let model = upstream_model
        .or(record.model.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let service_tier = metadata_service_tier(&record.metadata);
    let breakdown = billing::calculate_cost_breakdown(
        input_tokens,
        output_tokens,
        cached_tokens,
        model,
        service_tier,
    );
    let original_cost = breakdown.input_cost + breakdown.output_cost + breakdown.cache_read_cost;

    Some(AdminUsageRecordCostDetails {
        input_cost: breakdown.input_cost,
        output_cost: breakdown.output_cost,
        cache_read_cost: breakdown.cache_read_cost,
        total_cost: breakdown.total_cost,
        billed_cost: breakdown.total_cost,
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
