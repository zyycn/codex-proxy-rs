//! 编译插件观察订阅，投影请求事实并调度有界事件通知

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gateway_admin::model::{
    AdminError,
    plugins::instances::{PluginCapabilityBinding, PluginFailurePolicy},
};
use gateway_core::{
    engine::observation::{
        RequestObservation, RequestObservationOutcome, RequestObserverPlan,
        WebSocketResponseObservation,
    },
    metering::{CostEstimateStatus, CostSource},
    runtime::extensions::ExtensionSetReference,
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::{
    Capability, Manifest, SendState, Stage,
    call::observation::{
        Event, EventKind, RequestCompleted, RequestCost, RequestCostSource, RequestCostStatus,
        RequestFailure, RequestMoney, RequestOutcome, RequestTerminal, RequestTimings,
        RequestUsage, WebSocketResponse,
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use crate::{RpcSession, adapter::scope::BindingScope};

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(2);
const OBSERVATION_ENVELOPE_RESERVE: usize = 16 * 1024;

pub(crate) struct ObserverEntry {
    order: i32,
    plugin_id: String,
    instance_id: String,
    session: Arc<RpcSession>,
    callbacks: Arc<crate::callback::PluginCallbacks>,
    completed: Option<BindingScope>,
    websocket: Option<BindingScope>,
}

impl ObserverEntry {
    fn matches_completed(&self, observation: &RequestObservation) -> bool {
        self.completed
            .as_ref()
            .is_some_and(|scope| scope.matches_observation(observation))
    }

    async fn observe(
        &self,
        event: Event,
        payload: Vec<u8>,
        extensions: gateway_core::engine::extensions::ExtensionCallScope,
        timeout: Duration,
    ) {
        let request_id = event.request_id();
        let mut context = self.session.context(Stage::Observation, timeout);
        context.request_id = Some(request_id.to_owned());
        let Ok(_scope) = self.callbacks.prepare_observation(&context, extensions) else {
            return;
        };
        let Ok(params) = serde_json::to_value(&event) else {
            tracing::warn!(
                plugin_id = self.plugin_id,
                request_id,
                "插件观察事件编码失败"
            );
            return;
        };
        match self
            .session
            .call("observer.observe", context, params, payload)
            .await
        {
            Ok(reply)
                if reply.payload.is_empty()
                    && (reply.result.is_null()
                        || reply
                            .result
                            .as_object()
                            .is_some_and(serde_json::Map::is_empty)) => {}
            Ok(_) => tracing::warn!(
                plugin_id = self.plugin_id,
                instance_id = self.instance_id,
                request_id,
                "插件观察返回了无效结果"
            ),
            Err(error) => {
                tracing::warn!(plugin_id = self.plugin_id, instance_id = self.instance_id, request_id, %error, "插件观察失败")
            }
        }
    }
}

struct CompiledBindings {
    order: i32,
    completed: Option<BindingScope>,
    websocket: Option<BindingScope>,
}

fn compile_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<Option<CompiledBindings>, AdminError> {
    let mut completed = None;
    let mut websocket = None;
    let mut order = None;
    for binding in bindings {
        let capability = crate::contribution::resolve(manifest, binding)?.capability;
        if capability != Capability::Observer {
            continue;
        }
        let stage: Stage = serde_json::from_value(serde_json::Value::String(binding.stage.clone()))
            .map_err(|_| AdminError::invalid("插件观察阶段无效"))?;
        if stage != Stage::Observation || binding.failure_policy != PluginFailurePolicy::Observe {
            return Err(AdminError::invalid("观察绑定必须使用 observation/observe"));
        }
        if order.is_some_and(|existing| existing != binding.order) {
            return Err(AdminError::invalid("同一实例的观察绑定必须使用相同顺序"));
        }
        order = Some(binding.order);
        let event = binding.event.as_ref().and_then(|event| {
            serde_json::from_value::<EventKind>(serde_json::Value::String(event.clone())).ok()
        });
        let target = match event {
            Some(EventKind::RequestCompleted) => &mut completed,
            Some(EventKind::WebSocketResponse) => &mut websocket,
            None => return Err(AdminError::invalid("观察绑定必须选择有效事件类型")),
        };
        if target.replace(BindingScope::compile(binding)?).is_some() {
            return Err(AdminError::invalid("同一观察事件不能重复绑定"));
        }
    }
    Ok(order.map(|order| CompiledBindings {
        order,
        completed,
        websocket,
    }))
}

pub(crate) fn validate_bindings(
    manifest: &Manifest,
    bindings: &[PluginCapabilityBinding],
) -> Result<(), AdminError> {
    compile_bindings(manifest, bindings).map(drop)
}

pub(crate) fn compile_entry(
    manifest: &Manifest,
    plugin_id: &str,
    instance_id: &str,
    bindings: &[PluginCapabilityBinding],
    session: Arc<RpcSession>,
    callbacks: Arc<crate::callback::PluginCallbacks>,
) -> Result<Option<ObserverEntry>, AdminError> {
    Ok(
        compile_bindings(manifest, bindings)?.map(|compiled| ObserverEntry {
            order: compiled.order,
            plugin_id: plugin_id.to_owned(),
            instance_id: instance_id.to_owned(),
            session,
            callbacks,
            completed: compiled.completed,
            websocket: compiled.websocket,
        }),
    )
}

pub(crate) struct PluginObserverPlan {
    entries: Arc<[ObserverEntry]>,
    slots: Arc<Semaphore>,
    websocket_worker_slots: Arc<Semaphore>,
    websocket_payload_slots: Arc<Semaphore>,
    websocket_queues: Arc<Mutex<BTreeMap<String, mpsc::Sender<QueuedWebSocketObservation>>>>,
    websocket_queue_capacity: usize,
    maximum_websocket_payload_bytes: usize,
    timeout: Duration,
}

struct QueuedWebSocketObservation {
    observation: WebSocketResponseObservation,
    payload: Arc<[u8]>,
    entry_indexes: Vec<usize>,
    _payload_slot: OwnedSemaphorePermit,
}

impl PluginObserverPlan {
    pub(crate) fn compile(
        mut entries: Vec<ObserverEntry>,
        maximum_calls: usize,
        maximum_call_timeout: Duration,
        maximum_buffered_body_bytes: usize,
    ) -> Option<Arc<Self>> {
        if entries.is_empty() {
            return None;
        }
        entries.sort_by(|left, right| {
            (left.order, &left.plugin_id, &left.instance_id).cmp(&(
                right.order,
                &right.plugin_id,
                &right.instance_id,
            ))
        });
        let maximum_websocket_payload_bytes = maximum_buffered_body_bytes
            .saturating_sub(OBSERVATION_ENVELOPE_RESERVE.min(maximum_buffered_body_bytes));
        Some(Arc::new(Self {
            entries: entries.into(),
            slots: Arc::new(Semaphore::new(maximum_calls)),
            websocket_worker_slots: Arc::new(Semaphore::new(maximum_calls)),
            websocket_payload_slots: Arc::new(Semaphore::new(
                maximum_websocket_payload_bytes.max(1),
            )),
            websocket_queues: Arc::new(Mutex::new(BTreeMap::new())),
            websocket_queue_capacity: maximum_calls.clamp(1, 256),
            maximum_websocket_payload_bytes,
            timeout: maximum_call_timeout.min(OBSERVATION_TIMEOUT),
        }))
    }

    fn close_websocket_queue(&self, request_id: &str) {
        self.websocket_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(request_id);
    }
}

impl RequestObserverPlan for PluginObserverPlan {
    fn dispatch_websocket_response(
        &self,
        generation: ExtensionSetReference,
        observation: WebSocketResponseObservation,
    ) {
        let entry_indexes = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (!observation.suppresses_plugin(&entry.instance_id))
                    .then_some(entry)
                    .and_then(|entry| entry.websocket.as_ref())
                    .is_some_and(|scope| scope.matches_websocket_observation(&observation))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if entry_indexes.is_empty() {
            return;
        }
        let payload = match websocket_payload(&observation) {
            Ok(payload) => payload,
            Err(()) => {
                tracing::warn!(
                    request_id = observation.request_id().as_str(),
                    sequence = observation.sequence(),
                    "插件 WebSocket 响应观察编码失败，已丢弃"
                );
                return;
            }
        };
        if payload.len() > self.maximum_websocket_payload_bytes {
            tracing::warn!(
                request_id = observation.request_id().as_str(),
                sequence = observation.sequence(),
                payload_bytes = payload.len(),
                "插件 WebSocket 响应观察载荷超过上限，已丢弃"
            );
            return;
        }
        let Ok(payload_permits) = u32::try_from(payload.len().max(1)) else {
            return;
        };
        let Ok(payload_slot) = self
            .websocket_payload_slots
            .clone()
            .try_acquire_many_owned(payload_permits)
        else {
            tracing::warn!(
                request_id = observation.request_id().as_str(),
                sequence = observation.sequence(),
                "插件 WebSocket 响应观察载荷队列已满，已丢弃"
            );
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                request_id = observation.request_id().as_str(),
                "插件 WebSocket 响应观察缺少异步运行时，已丢弃"
            );
            return;
        };
        let request_id = observation.request_id().as_str().to_owned();
        let Some(sender) = self.websocket_sender(&handle, &request_id, generation) else {
            tracing::warn!(request_id, "插件 WebSocket 响应观察并发已满，已丢弃");
            return;
        };
        let sequence = observation.sequence();
        if sender
            .try_send(QueuedWebSocketObservation {
                observation,
                payload: payload.into(),
                entry_indexes,
                _payload_slot: payload_slot,
            })
            .is_err()
        {
            tracing::warn!(
                request_id,
                sequence,
                "插件 WebSocket 响应观察事件队列已满，已丢弃"
            );
        }
    }

    fn dispatch(&self, generation: ExtensionSetReference, observation: RequestObservation) {
        self.close_websocket_queue(observation.request_id().as_str());
        if !self.entries.iter().any(|entry| {
            !observation.suppresses_plugin(&entry.instance_id)
                && entry.matches_completed(&observation)
        }) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                request_id = observation.request_id().as_str(),
                "插件终态观察缺少异步运行时，已丢弃"
            );
            return;
        };
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            tracing::warn!(
                request_id = observation.request_id().as_str(),
                "插件终态观察并发已满，已丢弃"
            );
            return;
        };
        let entries = Arc::clone(&self.entries);
        let timeout = self.timeout;
        handle.spawn(async move {
            let _generation = generation;
            let _slot = slot;
            for entry in entries.iter() {
                if observation.suppresses_plugin(&entry.instance_id) {
                    continue;
                }
                if !entry.matches_completed(&observation) {
                    continue;
                }
                entry
                    .observe(
                        Event::RequestCompleted(Box::new(wire_observation(&observation))),
                        Vec::new(),
                        observation.extension_scope().clone(),
                        timeout,
                    )
                    .await;
            }
        });
    }
}

impl PluginObserverPlan {
    fn websocket_sender(
        &self,
        handle: &tokio::runtime::Handle,
        request_id: &str,
        generation: ExtensionSetReference,
    ) -> Option<mpsc::Sender<QueuedWebSocketObservation>> {
        let mut queues = self
            .websocket_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sender) = queues.get(request_id) {
            return Some(sender.clone());
        }
        let worker_slot = self
            .websocket_worker_slots
            .clone()
            .try_acquire_owned()
            .ok()?;
        let (sender, receiver) = mpsc::channel(self.websocket_queue_capacity);
        queues.insert(request_id.to_owned(), sender.clone());
        let entries = Arc::clone(&self.entries);
        let timeout = self.timeout;
        handle.spawn(dispatch_websocket_queue(
            entries,
            timeout,
            generation,
            worker_slot,
            receiver,
        ));
        Some(sender)
    }
}

async fn dispatch_websocket_queue(
    entries: Arc<[ObserverEntry]>,
    timeout: Duration,
    generation: ExtensionSetReference,
    worker_slot: OwnedSemaphorePermit,
    mut receiver: mpsc::Receiver<QueuedWebSocketObservation>,
) {
    let _generation = generation;
    let _worker_slot = worker_slot;
    while let Some(queued) = receiver.recv().await {
        for index in queued.entry_indexes.iter().copied() {
            let entry = &entries[index];
            let include_payload = !queued.payload.is_empty();
            let request = WebSocketResponse {
                event_id: queued.observation.event_id().to_owned(),
                request_id: queued.observation.request_id().as_str().to_owned(),
                config_revision: queued.observation.config_revision().get(),
                operation: queued.observation.operation().as_str().to_owned(),
                protocol: queued.observation.wire().protocol().to_owned(),
                provider: queued.observation.provider().as_str().to_owned(),
                attempt_index: queued.observation.attempt_index().get(),
                sequence: queued.observation.sequence(),
                payload_included: include_payload,
                requested_model: queued
                    .observation
                    .requested_model()
                    .map(|model| model.as_str().to_owned()),
                account_id: Some(queued.observation.account_id().as_str().to_owned()),
                event_type: include_payload
                    .then(|| queued.observation.wire().event_type())
                    .flatten()
                    .map(str::to_owned),
            };
            entry
                .observe(
                    Event::WebSocketResponse(Box::new(request)),
                    queued.payload.to_vec(),
                    queued.observation.extension_scope().clone(),
                    timeout,
                )
                .await;
        }
    }
}

fn websocket_payload(observation: &WebSocketResponseObservation) -> Result<Vec<u8>, ()> {
    if let Some(raw) = observation.wire().raw_json_body() {
        return Ok(raw.to_vec());
    }
    if observation.wire().has_json_data() {
        return serde_json::to_vec(observation.wire().data()).map_err(|_| ());
    }
    Ok(Vec::new())
}

fn wire_observation(observation: &RequestObservation) -> RequestCompleted {
    RequestCompleted {
        event_id: observation.event_id().to_owned(),
        request_id: observation.request_id().as_str().to_owned(),
        config_revision: observation.config_revision().get(),
        operation: observation.operation().as_str().to_owned(),
        client_key_id: Some(observation.client_key_id().as_str().to_owned()),
        account_id: observation
            .account_id()
            .map(|account| account.as_str().to_owned()),
        upstream_model: observation
            .upstream_model()
            .map(|model| model.as_str().to_owned()),
        response_model: observation.response_model().map(str::to_owned),
        service_tier: observation.service_tier().map(str::to_owned),
        requested_model: observation
            .requested_model()
            .map(|model| model.as_str().to_owned()),
        provider: observation
            .provider()
            .map(|provider| provider.as_str().to_owned()),
        completed_at_ms: millis_since_epoch(observation.completed_at()),
        terminal: RequestTerminal {
            outcome: wire_outcome(observation.outcome()),
            send_state: wire_send_state(observation.send_state()),
            attempt_count: observation.attempt_count(),
            client_status_code: observation.client_status_code(),
            error_code: observation
                .error_kind()
                .map(|error| error.as_str().to_owned()),
        },
        usage: {
            let usage = observation.usage();
            RequestUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_tokens: usage.cached_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                image_input_tokens: usage.image_input_tokens,
                image_output_tokens: usage.image_output_tokens,
                total_tokens: usage.total_tokens,
                cost: Some(wire_cost(observation)),
                timings: Some(wire_timings(observation)),
                failure: wire_failure(observation),
            }
        },
    }
}

fn wire_outcome(outcome: RequestObservationOutcome) -> RequestOutcome {
    match outcome {
        RequestObservationOutcome::Succeeded => RequestOutcome::Succeeded,
        RequestObservationOutcome::Failed => RequestOutcome::Failed,
        RequestObservationOutcome::Rejected => RequestOutcome::Rejected,
        RequestObservationOutcome::Cancelled => RequestOutcome::Cancelled,
        RequestObservationOutcome::Incomplete => RequestOutcome::Incomplete,
    }
}

fn wire_send_state(send_state: UpstreamSendState) -> SendState {
    match send_state {
        UpstreamSendState::NotSent => SendState::NotSent,
        UpstreamSendState::Sent => SendState::Sent,
        UpstreamSendState::Ambiguous => SendState::Ambiguous,
    }
}

fn wire_cost(observation: &RequestObservation) -> RequestCost {
    let cost = observation.cost();
    RequestCost {
        status: match cost.status() {
            CostEstimateStatus::Known => RequestCostStatus::Known,
            CostEstimateStatus::Unknown => RequestCostStatus::Unknown,
        },
        source: match cost.source() {
            CostSource::ProviderReported => RequestCostSource::ProviderReported,
            CostSource::Calculated => RequestCostSource::Calculated,
            CostSource::Unavailable => RequestCostSource::Unavailable,
        },
        total: cost.total().map(|total| RequestMoney {
            amount: total.amount().canonical(),
            currency: total.currency().as_str().to_owned(),
        }),
    }
}

fn wire_timings(observation: &RequestObservation) -> RequestTimings {
    let timings = observation.timings();
    RequestTimings {
        transport_decision_wait_ms: timings.transport_decision_wait_ms,
        connect_ms: timings.connect_ms,
        headers_ms: timings.headers_ms,
        first_event_ms: timings.first_event_ms,
        first_reasoning_ms: timings.first_reasoning_ms,
        first_text_ms: timings.first_text_ms,
        first_token_ms: timings.first_token_ms,
        provider_processing_ms: timings.provider_processing_ms,
        latency_ms: timings.latency_ms,
    }
}

fn wire_failure(observation: &RequestObservation) -> Option<RequestFailure> {
    (observation.outcome() != RequestObservationOutcome::Succeeded).then(|| RequestFailure {
        outcome: wire_outcome(observation.outcome()),
        send_state: wire_send_state(observation.send_state()),
        attempt_count: observation.attempt_count(),
        client_status_code: observation.client_status_code(),
        upstream_status_code: observation.upstream_status_code(),
        error_code: observation
            .error_kind()
            .map(|error| error.as_str().to_owned()),
        retry_after_ms: observation.retry_after_ms(),
    })
}

fn millis_since_epoch(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
