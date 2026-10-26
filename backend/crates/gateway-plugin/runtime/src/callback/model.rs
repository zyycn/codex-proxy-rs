//! 模型目录与执行适配；身份解析交给客户端端口，路由与计费沿用 Core

use std::sync::{Arc, OnceLock, Weak};

use bytes::Bytes;
use gateway_admin::model::AdminError;
use gateway_core::{
    account::ProviderAccountId,
    engine::{
        CommitRequirement, ModelRequestId,
        execution::{
            AuthenticatedClient, ClientTransport, ExecutionRequestMetadata, ExecutionSession,
        },
        nested::{
            BoundModelExecutionRequest, NestedModelExecutionPort, NestedModelExecutionRequest,
        },
    },
    event::ProviderEvent,
    identity::ProviderKind,
    operation::{
        GenerateRequest, ImageRequest, ImageRequestKind, Operation, ProtocolPayload,
        RawJsonPayload, StandaloneSearchRequest,
    },
    routing::PublicModelId,
};
use gateway_plugin_sdk::{
    CallContext, ErrorCode, PluginFault,
    call::host::{
        self as wire, ModelEventBatch, ModelExecuteRequest, ModelExecuteResult, ModelListRequest,
        ModelListResult, ModelOperation, ModelStreamCloseRequest, ModelStreamReadRequest,
        ModelStreamReadResult, ModelStreamResult,
    },
};

use super::{CallResources, denied, invalid};
use crate::RpcReply;

const MAX_MODEL_STREAMS_PER_CALL: usize = 16;

pub(crate) struct PluginModelPortSlot {
    port: OnceLock<Weak<dyn NestedModelExecutionPort>>,
}

impl PluginModelPortSlot {
    pub(crate) const fn new() -> Self {
        Self {
            port: OnceLock::new(),
        }
    }

    pub(crate) fn bind(&self, port: &Arc<dyn NestedModelExecutionPort>) -> Result<(), AdminError> {
        self.port
            .set(Arc::downgrade(port))
            .map_err(|_| AdminError::conflict("插件模型执行端口已经绑定"))
    }

    fn upgrade(&self) -> Result<Arc<dyn NestedModelExecutionPort>, PluginFault> {
        self.port.get().and_then(Weak::upgrade).ok_or_else(denied)
    }
}

pub(super) struct PluginModels {
    slot: Arc<PluginModelPortSlot>,
    clients: Arc<super::client::PluginClients>,
    request_limits: Arc<super::request_limits::RequestLimits>,
    maximum_payload: usize,
}

impl PluginModels {
    pub(super) fn new(
        slot: Arc<PluginModelPortSlot>,
        clients: Arc<super::client::PluginClients>,
        request_limits: Arc<super::request_limits::RequestLimits>,
        maximum_payload: usize,
    ) -> Self {
        Self {
            slot,
            clients,
            request_limits,
            maximum_payload,
        }
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        call: &Arc<CallResources>,
        method: &str,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcReply, PluginFault> {
        match method {
            wire::MODELS_LIST => {
                if !payload.is_empty() {
                    return Err(invalid());
                }
                let request: ModelListRequest =
                    serde_json::from_value(params).map_err(|_| invalid())?;
                if request.protocol.is_empty()
                    || request.protocol.len() > 64
                    || request.client_version.len() > 128
                {
                    return Err(invalid());
                }
                let port = self.slot.upgrade()?;
                let client = self.clients.identity(call, request.client_key_id).await?;
                let models = port
                    .models(
                        client,
                        call.cancellation.clone(),
                        request.protocol,
                        request.client_version,
                    )
                    .await
                    .map_err(super::error::gateway)?;
                encode_result(ModelListResult {
                    models: models
                        .into_iter()
                        .map(|model| model.as_str().to_owned())
                        .collect(),
                })
            }
            wire::MODEL_EXECUTE | wire::MODEL_EXECUTE_STREAM => {
                let mut request: ModelExecuteRequest =
                    serde_json::from_value(params).map_err(|_| invalid())?;
                if payload.is_empty() {
                    return Err(invalid());
                }
                let check_size = |limit| {
                    if payload.len() > gateway_plugin_sdk::call::model::request_payload_limit(limit)
                    {
                        Err(super::request_limits::too_large())
                    } else {
                        Ok(())
                    }
                };
                let port = self.slot.upgrade()?;
                let stream = method == wire::MODEL_EXECUTE_STREAM;
                let started = if let Some(client_key_id) = request.client_key_id.take() {
                    let client = self.clients.identity(call, client_key_id).await?;
                    check_size(client.snapshot().responses_max_decompressed_body_bytes())?;
                    let request = self.request_parts(request, payload, stream)?;
                    let request = Self::bound_request(context, call, request, client);
                    call.observe_dispatch()?;
                    port.start_bound(request)
                        .await
                        .map_err(super::error::gateway)?
                } else {
                    check_size(self.request_limits.body_bytes(call)?)?;
                    let request = self.request(context, request, payload, stream)?;
                    port.start(request).await.map_err(super::error::gateway)?
                };
                if method == wire::MODEL_EXECUTE {
                    execute_buffered(started, self.maximum_payload).await
                } else {
                    let stream = Arc::new(ModelStream::new(started.session, self.maximum_payload));
                    let id = uuid::Uuid::new_v4().to_string();
                    let mut state = call
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.closed || state.model_streams.len() >= MAX_MODEL_STREAMS_PER_CALL {
                        drop(state);
                        stream.close();
                        return Err(PluginFault::new(
                            ErrorCode::Capacity,
                            "model stream capacity is exhausted",
                        ));
                    }
                    state.model_streams.insert(id.clone(), stream);
                    encode_result(ModelStreamResult {
                        request_id: started.request_id.as_str().to_owned(),
                        stream: id,
                    })
                }
            }
            wire::MODEL_STREAM_READ => {
                if !payload.is_empty() {
                    return Err(invalid());
                }
                let request: ModelStreamReadRequest =
                    serde_json::from_value(params).map_err(|_| invalid())?;
                if request.maximum_bytes == 0
                    || usize::try_from(request.maximum_bytes)
                        .map_or(true, |maximum| maximum > self.maximum_payload)
                {
                    return Err(invalid());
                }
                let stream = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .model_streams
                    .get(&request.stream)
                    .cloned()
                    .ok_or_else(denied)?;
                let read = stream
                    .read(usize::try_from(request.maximum_bytes).map_err(|_| invalid())?)
                    .await?;
                if read.end {
                    call.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .model_streams
                        .remove(&request.stream);
                }
                Ok(RpcReply {
                    result: serde_json::to_value(ModelStreamReadResult {
                        events: read.events,
                        end: read.end,
                    })
                    .map_err(|_| invalid())?,
                    payload: read.payload,
                })
            }
            wire::MODEL_STREAM_CLOSE => {
                if !payload.is_empty() {
                    return Err(invalid());
                }
                let request: ModelStreamCloseRequest =
                    serde_json::from_value(params).map_err(|_| invalid())?;
                let stream = call
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .model_streams
                    .remove(&request.stream)
                    .ok_or_else(denied)?;
                stream.close();
                Ok(RpcReply {
                    result: serde_json::json!({}),
                    payload: Vec::new(),
                })
            }
            _ => Err(denied()),
        }
    }

    fn request(
        &self,
        context: &CallContext,
        request: ModelExecuteRequest,
        body: Vec<u8>,
        stream: bool,
    ) -> Result<NestedModelExecutionRequest, PluginFault> {
        let parent_request_id =
            context
                .request_id
                .as_ref()
                .ok_or_else(invalid)
                .and_then(|request_id| {
                    ModelRequestId::new(request_id.clone()).map_err(|_| invalid())
                })?;
        let parent_account = context
            .account_id
            .as_ref()
            .map(|account| ProviderAccountId::new(account.clone()))
            .transpose()
            .map_err(|_| denied())?;
        let request = self.request_parts(request, body, stream)?;
        Ok(NestedModelExecutionRequest {
            parent_request_id,
            initiating_plugin_instance_id: context.instance_id.clone(),
            public_model: request.public_model,
            operation: request.operation,
            metadata: request.metadata,
            provider: request.provider,
            account: request.account,
            parent_account,
        })
    }

    fn bound_request(
        context: &CallContext,
        call: &CallResources,
        request: ModelRequestParts,
        client: AuthenticatedClient,
    ) -> BoundModelExecutionRequest {
        BoundModelExecutionRequest {
            client,
            initiating_plugin_instance_id: context.instance_id.clone(),
            cancellation: call.cancellation.clone(),
            extension_scope: call.scope.extension_scope.clone(),
            public_model: request.public_model,
            operation: request.operation,
            metadata: request.metadata,
            provider: request.provider,
            account: request.account,
        }
    }

    fn request_parts(
        &self,
        request: ModelExecuteRequest,
        body: Vec<u8>,
        stream: bool,
    ) -> Result<ModelRequestParts, PluginFault> {
        let provider = request
            .provider
            .map(ProviderKind::new)
            .transpose()
            .map_err(|_| invalid())?;
        let account = request
            .account_id
            .map(ProviderAccountId::new)
            .transpose()
            .map_err(|_| invalid())?;
        let previous_response_id = request
            .previous_response_id
            .map(gateway_core::engine::continuation::PreviousResponseId::new);
        let operation = decode_operation(
            request.operation,
            request.protocol.clone(),
            body,
            previous_response_id.as_ref().map(|id| id.as_str()),
        )?;
        Ok(ModelRequestParts {
            public_model: PublicModelId::new(request.model).map_err(|_| invalid())?,
            operation,
            metadata: ExecutionRequestMetadata {
                protocol: request.protocol,
                endpoint: "host.model".to_owned(),
                transport: ClientTransport::InternalPlugin,
                stream,
                client_ip: None,
                user_agent: None,
                previous_response_id,
            },
            provider,
            account,
        })
    }
}

struct ModelRequestParts {
    public_model: PublicModelId,
    operation: Operation,
    metadata: ExecutionRequestMetadata,
    provider: Option<ProviderKind>,
    account: Option<ProviderAccountId>,
}

fn decode_operation(
    operation: ModelOperation,
    protocol: String,
    body: Vec<u8>,
    previous_response_id: Option<&str>,
) -> Result<Operation, PluginFault> {
    if serde_json::from_slice::<serde_json::Value>(&body).is_err() {
        return Err(invalid());
    }
    match operation {
        ModelOperation::Generate => {
            let mut body =
                serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&body)
                    .map_err(|_| invalid())?;
            if let Some(previous) = previous_response_id {
                match body.get("previous_response_id") {
                    Some(value) if value.as_str() != Some(previous) => return Err(invalid()),
                    Some(_) => {}
                    None => {
                        body.insert(
                            "previous_response_id".to_owned(),
                            serde_json::Value::String(previous.to_owned()),
                        );
                    }
                }
            }
            Ok(Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(protocol, body).map_err(|_| invalid())?,
            )))
        }
        ModelOperation::GenerateImage | ModelOperation::EditImage => {
            let kind = if operation == ModelOperation::GenerateImage {
                ImageRequestKind::Generation
            } else {
                ImageRequestKind::Edit
            };
            Ok(Operation::GenerateImage(ImageRequest::from_raw_json(
                kind,
                RawJsonPayload::new(protocol, Bytes::from(body)).map_err(|_| invalid())?,
            )))
        }
        ModelOperation::Search => Ok(Operation::Search(StandaloneSearchRequest::from_raw_json(
            RawJsonPayload::new(protocol, Bytes::from(body)).map_err(|_| invalid())?,
        ))),
    }
}

async fn execute_buffered(
    mut started: gateway_core::engine::execution::StartedExecution,
    maximum_payload: usize,
) -> Result<RpcReply, PluginFault> {
    let events = match started.session.collect_uncommitted().await {
        Ok(events) => events,
        Err(error) => {
            let fault = super::error::engine(&error);
            started.session.detach_finalize().await;
            return Err(fault);
        }
    };
    let (payload, events) = match encode_events(events) {
        Ok(encoded) => encoded,
        Err(error) => {
            started.session.detach_finalize().await;
            return Err(error);
        }
    };
    if payload.len() > maximum_payload {
        started.session.detach_finalize().await;
        return Err(PluginFault::new(
            ErrorCode::Capacity,
            "model response requires streaming",
        ));
    }
    if let Err(error) = started.session.commit_downstream(Some(200)).await {
        let fault = super::error::engine(&error);
        started.session.detach_finalize().await;
        return Err(fault);
    }
    if !started.session.is_finalized() {
        started.session.detach_finalize().await;
        return Err(PluginFault::new(
            ErrorCode::Fault,
            "model execution did not finalize",
        ));
    }
    Ok(RpcReply {
        result: serde_json::to_value(ModelExecuteResult {
            request_id: started.request_id.as_str().to_owned(),
            events,
        })
        .map_err(|_| invalid())?,
        payload,
    })
}

struct PendingBatch {
    payload: Vec<u8>,
    events: u32,
    commit: bool,
}

struct ModelStreamState {
    session: Option<Box<dyn ExecutionSession>>,
    pending: Option<PendingBatch>,
}

pub(super) struct ModelStream {
    state: tokio::sync::Mutex<ModelStreamState>,
    closed: tokio::sync::watch::Sender<bool>,
    maximum_payload: usize,
}

struct ModelRead {
    payload: Vec<u8>,
    events: u32,
    end: bool,
}

impl ModelStream {
    fn new(session: Box<dyn ExecutionSession>, maximum_payload: usize) -> Self {
        Self {
            state: tokio::sync::Mutex::new(ModelStreamState {
                session: Some(session),
                pending: None,
            }),
            closed: tokio::sync::watch::channel(false).0,
            maximum_payload,
        }
    }

    async fn read(&self, maximum_bytes: usize) -> Result<ModelRead, PluginFault> {
        let mut state = self.state.try_lock().map_err(|_| {
            PluginFault::new(ErrorCode::Capacity, "model stream already has a reader")
        })?;
        loop {
            let is_closed = *self.closed.borrow();
            if is_closed {
                let session = state.session.take();
                state.pending.take();
                drop(state);
                if let Some(session) = session {
                    session.cancel();
                    session.detach_finalize().await;
                }
                return Err(PluginFault::new(
                    ErrorCode::Cancelled,
                    "model stream was closed",
                ));
            }
            if let Some(pending) = state.pending.take() {
                if pending.payload.len() > maximum_bytes {
                    state.pending = Some(pending);
                    return Err(PluginFault::new(
                        ErrorCode::Capacity,
                        "model event exceeds the requested read size",
                    ));
                }
                if pending.commit {
                    let result = state
                        .session
                        .as_mut()
                        .ok_or_else(denied)?
                        .commit_downstream(Some(200))
                        .await;
                    if let Err(error) = result {
                        let fault = super::error::engine(&error);
                        let session = state.session.take();
                        drop(state);
                        if let Some(session) = session {
                            session.cancel();
                            session.detach_finalize().await;
                        }
                        return Err(fault);
                    }
                }
                return Ok(ModelRead {
                    payload: pending.payload,
                    events: pending.events,
                    end: false,
                });
            }
            let mut closed = self.closed.subscribe();
            let next = {
                let session = state.session.as_mut().ok_or_else(denied)?;
                tokio::select! {
                    biased;
                    _ = closed.wait_for(|closed| *closed) => {
                        session.cancel();
                        None
                    }
                    result = session.next_event() => Some(result),
                }
            };
            let Some(next) = next else {
                let session = state.session.take();
                drop(state);
                if let Some(session) = session {
                    session.cancel();
                    session.detach_finalize().await;
                }
                return Err(PluginFault::new(
                    ErrorCode::Cancelled,
                    "model stream was closed",
                ));
            };
            match next {
                Ok(Some(event)) => {
                    let commit =
                        event.commit_requirement() == CommitRequirement::CommitBeforeDelivery;
                    let (payload, events) = match encode_events(event.into_provider_events()) {
                        Ok(encoded) => encoded,
                        Err(error) => {
                            let session = state.session.take();
                            drop(state);
                            if let Some(session) = session {
                                session.cancel();
                                session.detach_finalize().await;
                            }
                            return Err(error);
                        }
                    };
                    if events == 0 {
                        // 空批次仍需释放待交付屏障，已经提交的空批次直接继续拉取
                        if commit {
                            let result = state
                                .session
                                .as_mut()
                                .ok_or_else(denied)?
                                .discard_pending_delivery();
                            if result.is_err() {
                                let session = state.session.take();
                                drop(state);
                                if let Some(session) = session {
                                    session.cancel();
                                    session.detach_finalize().await;
                                }
                                return Err(PluginFault::new(
                                    ErrorCode::Fault,
                                    "model event cannot be discarded",
                                ));
                            }
                        }
                        continue;
                    }
                    if payload.len() > self.maximum_payload {
                        let session = state.session.take();
                        drop(state);
                        if let Some(session) = session {
                            session.cancel();
                            session.detach_finalize().await;
                        }
                        return Err(PluginFault::new(
                            ErrorCode::Capacity,
                            "model event exceeds the stream payload limit",
                        ));
                    }
                    state.pending = Some(PendingBatch {
                        payload,
                        events,
                        commit,
                    });
                }
                Ok(None) => {
                    let finalized = state
                        .session
                        .as_ref()
                        .is_some_and(|session| session.is_finalized());
                    let session = state.session.take();
                    drop(state);
                    if !finalized {
                        if let Some(session) = session {
                            session.cancel();
                            session.detach_finalize().await;
                        }
                        return Err(PluginFault::new(
                            ErrorCode::Fault,
                            "model stream ended before finalization",
                        ));
                    }
                    return Ok(ModelRead {
                        payload: Vec::new(),
                        events: 0,
                        end: true,
                    });
                }
                Err(error) => {
                    let fault = super::error::engine(&error);
                    let session = state.session.take();
                    drop(state);
                    if let Some(session) = session {
                        session.cancel();
                        session.detach_finalize().await;
                    }
                    return Err(fault);
                }
            }
        }
    }

    pub(super) fn close(self: &Arc<Self>) {
        self.closed.send_replace(true);
        let stream = Arc::clone(self);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                let session = stream.state.lock().await.session.take();
                if let Some(session) = session {
                    session.cancel();
                    session.detach_finalize().await;
                }
            }));
        }
    }
}

fn encode_events(events: Vec<ProviderEvent>) -> Result<(Vec<u8>, u32), PluginFault> {
    let events = events
        .into_iter()
        .map(super::facts::event)
        .collect::<Result<Vec<_>, _>>()?;
    let count = u32::try_from(events.len()).map_err(|_| invalid())?;
    if events.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let payload = ModelEventBatch { events }
        .encode()
        .map_err(|_| PluginFault::new(ErrorCode::Capacity, "model event batch is too large"))?;
    Ok((payload, count))
}

fn encode_result(value: impl serde::Serialize) -> Result<RpcReply, PluginFault> {
    Ok(RpcReply {
        result: serde_json::to_value(value).map_err(|_| invalid())?,
        payload: Vec::new(),
    })
}
