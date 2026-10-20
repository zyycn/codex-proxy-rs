//! 宿主确认的执行事实快照
//! 读取快照不会取得原执行的计费或资源所有权

use serde::{Deserialize, Serialize};

use super::WireEvent;
use crate::call::{middleware::MiddlewareHeader, observation::RequestMoney};

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionFacts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub costs: Vec<ExecutionCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<ResponseObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_update: Option<SessionState>,
    pub middleware_transformed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub middleware_origin_wire: Option<WireEvent>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionCost {
    Calculated {
        total: RequestMoney,
        breakdown: Option<Box<CostBreakdown>>,
    },
    ProviderReported {
        total: RequestMoney,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostBreakdown {
    pub long_context_billing_applied: bool,
    pub image: Option<ImageCostBreakdown>,
    pub input_amount: RequestMoney,
    pub output_amount: RequestMoney,
    pub cache_read_amount: RequestMoney,
    pub cache_write_amount: RequestMoney,
    pub standard_amount: RequestMoney,
    pub total_amount: RequestMoney,
    pub input_price_per_million: RequestMoney,
    pub output_price_per_million: RequestMoney,
    pub cache_read_price_per_million: RequestMoney,
    pub cache_write_price_per_million: RequestMoney,
    pub service_tier: Option<String>,
    pub multiplier_percent: u32,
    pub custom_multiplier_bps: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageCostBreakdown {
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub input_amount: RequestMoney,
    pub cache_read_amount: RequestMoney,
    pub input_price_per_million: RequestMoney,
    pub cache_read_price_per_million: RequestMoney,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseObservation {
    pub transport: String,
    pub http_version: Option<String>,
    pub websocket_pool: Option<String>,
    pub status_code: Option<u16>,
    pub request_id: Option<String>,
    pub service_tier: Option<String>,
    pub upstream_response_model: Option<String>,
    #[serde(default)]
    pub timings: ResponseTimings,
    #[serde(default)]
    pub client_headers: Vec<MiddlewareHeader>,
    /// Provider 已产生的完整观测 JSON 文本，保留原始表示
    pub provider_metadata: Option<String>,
}

/// 原始上游计时；不包含最终总耗时、速率或费用等业务结果
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseTimings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_decision_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_event_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_reasoning_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_text_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_processing_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_response_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_api_overhead_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_engine_ms: Option<f64>,
}

impl ResponseObservation {
    #[must_use]
    pub fn new(transport: impl Into<String>) -> Self {
        Self {
            transport: transport.into(),
            http_version: None,
            websocket_pool: None,
            status_code: None,
            request_id: None,
            service_tier: None,
            upstream_response_model: None,
            timings: ResponseTimings::default(),
            client_headers: Vec::new(),
            provider_metadata: None,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
    pub provider: String,
    pub payload: serde_json::Map<String, serde_json::Value>,
    pub extension_owner: Option<SessionOwner>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionOwner {
    pub instance_id: String,
    pub contribution_id: String,
    pub adapter_id: String,
    pub generation: u64,
    pub incarnation: String,
    pub connection_local: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCallMetadata {
    pub provider: String,
    pub upstream_model: Option<String>,
    pub provider_account_id: String,
    pub upstream_request_id: Option<String>,
    pub transport: String,
    pub selection_observation: Option<SelectionObservation>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionObservation {
    pub account_selection_wait_ms: u64,
    pub capacity: Option<AccountCapacity>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountCapacity {
    pub used_slots: u64,
    pub total_slots: u64,
}
