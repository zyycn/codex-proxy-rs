//! 验证插件可读取执行事实而不替换或重复原始执行

mod contract;
use super::*;
use futures::StreamExt;
use gateway_core::{
    engine::provider::{
        ProviderCallMetadata, ProviderSelectionObservation, ProviderStream,
        execute_attempt_middleware,
    },
    event::{
        FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader,
        ProviderResponseMetadata, ProviderResponseObservation, ProviderResponseTimings,
        ResponseMeta,
    },
    metering::{
        CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates, Decimal, Money,
        ProviderReportedCost, Usage,
    },
    operation::{ExtensionSessionOwner, ProviderSessionState},
    routing::UpstreamModelId,
    upstream::{OpaqueUpstreamValue, UpstreamTransport},
};
