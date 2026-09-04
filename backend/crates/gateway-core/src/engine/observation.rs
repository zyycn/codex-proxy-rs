//! 单次响应的计量、时间与响应身份事实；重试丢弃时统一清理。

use super::ModelRequestTimings;
use crate::event::GatewayEvent;
use crate::event::ProviderResponseObservation;
use crate::metering::{CostEstimate, CostSource, Usage};
use std::time::Instant;

pub(super) struct ResponseObservation {
    pub(super) timing_started_at: Instant,
    pub(super) usage: Usage,
    pub(super) cost: CostEstimate,
    pub(super) timings: ModelRequestTimings,
    pub(super) client_response_id: Option<String>,
    pub(super) upstream_response_id: Option<String>,
}

impl ResponseObservation {
    pub(super) fn new(timing_started_at: Instant) -> Self {
        Self {
            timing_started_at,
            usage: Usage::new(),
            cost: CostEstimate::unavailable(),
            timings: ModelRequestTimings::default(),
            client_response_id: None,
            upstream_response_id: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.timing_started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn finish(&mut self) {
        self.timings.latency_ms = Some(self.elapsed_ms());
    }

    pub(super) fn observe_event(&mut self, event: &GatewayEvent) {
        let elapsed = self.elapsed_ms();
        observe_event_timing(&mut self.timings, event, elapsed);
        if let GatewayEvent::Usage(observed) = event {
            self.usage.merge(observed);
        }
        if let GatewayEvent::CalculatedCost(observed) = event
            && self.cost.source() != CostSource::ProviderReported
        {
            self.cost = observed.into_estimate();
        }
        if let GatewayEvent::ProviderCost(observed) = event {
            self.cost = observed.into_estimate();
        }
    }

    pub(super) fn observe_identity(&mut self, event: &GatewayEvent) {
        let metadata = match event {
            GatewayEvent::Started(metadata) | GatewayEvent::Completed(metadata) => metadata,
            _ => return,
        };
        let response_id = metadata.response_id().to_owned();
        self.client_response_id = Some(response_id.clone());
        self.upstream_response_id = Some(response_id);
    }

    pub(super) fn reset_for_attempt(&mut self) {
        self.usage = Usage::new();
        self.cost = CostEstimate::unavailable();
        self.client_response_id = None;
        self.upstream_response_id = None;
        self.timings.transport_decision_wait_ms = None;
        self.timings.connect_ms = None;
        self.timings.headers_ms = None;
        self.timings.first_event_ms = None;
        self.timings.first_reasoning_ms = None;
        self.timings.first_text_ms = None;
        self.timings.first_token_ms = None;
        self.timings.provider_processing_ms = None;
    }

    pub(super) fn observe_response(&mut self, observation: &ProviderResponseObservation) {
        let observed = observation.timings();
        if let Some(value) = observed.transport_decision_wait_ms {
            self.timings.transport_decision_wait_ms = Some(value);
        }
        if let Some(value) = observed.connect_ms {
            self.timings.connect_ms = Some(value);
        }
        if let Some(value) = observed.headers_ms {
            self.timings.headers_ms = Some(value);
        }
        if let Some(value) = observed.first_event_ms {
            self.timings.first_event_ms = Some(value);
        }
        if let Some(value) = observed.first_reasoning_ms {
            self.timings.first_reasoning_ms = Some(value);
        }
        if let Some(value) = observed.first_text_ms {
            self.timings.first_text_ms = Some(value);
        }
        if let Some(value) = observed.first_token_ms {
            self.timings.first_token_ms = Some(value);
        }
        if let Some(value) = observed.provider_processing_ms {
            self.timings.provider_processing_ms = Some(value);
        }
    }
}

fn observe_event_timing(timings: &mut ModelRequestTimings, event: &GatewayEvent, elapsed_ms: u64) {
    timings.first_event_ms.get_or_insert(elapsed_ms);
    match event {
        GatewayEvent::ReasoningDelta(_) => {
            timings.first_reasoning_ms.get_or_insert(elapsed_ms);
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        GatewayEvent::TextDelta(_) => {
            timings.first_text_ms.get_or_insert(elapsed_ms);
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        // `response.output_item.added` 会先投影一个空参数的 tool delta；它只是结构帧，
        // 不能抢在真实工具参数之前成为首个可消费 token。
        GatewayEvent::ToolCallDelta(delta) if !delta.arguments_delta.is_empty() => {
            timings.first_token_ms.get_or_insert(elapsed_ms);
        }
        GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_) => {}
        _ => {}
    }
}
