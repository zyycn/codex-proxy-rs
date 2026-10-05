//! 执行插件上游适配调用，管理事件流、续接状态与调用生命周期

use std::sync::Arc;

use futures::stream;
use gateway_core::{
    engine::{provider::EventStream, upstream_adapter::UpstreamAdapterInvocation},
    error::{ProviderError, ProviderErrorKind},
    operation::{ExtensionSessionOwner, ProviderSessionState},
    upstream::UpstreamSendState,
};
use gateway_plugin_sdk::{
    CallContext, ErrorCode, PluginFault, Stage,
    call::upstream_adapter::{
        ContinuationScope, EXECUTE_METHOD, UpstreamAdapterRequest, UpstreamContinuation,
        UpstreamTransport,
    },
};
use serde::{Deserialize, Serialize};

use super::{
    PluginUpstreamAdapter,
    event::{DecodedEvent, EventDecoder, invalid},
};
use crate::{
    RpcError, RpcStream,
    callback::{
        CallbackScope,
        upstream::{ConnectionOwner, ManagedUpstream},
    },
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredContinuation {
    continuation: UpstreamContinuation,
    client_key_id: String,
    account_id: String,
    credential_revision: u64,
    connection: Option<String>,
}

struct Execution {
    adapter: Arc<PluginUpstreamAdapter>,
    invocation: UpstreamAdapterInvocation,
    active: Option<ActiveCall>,
    decoder: EventDecoder,
    ended: bool,
}

struct ActiveCall {
    context: CallContext,
    stream: RpcStream,
    managed: Arc<ManagedUpstream>,
    _scope: Arc<CallbackScope>,
}

pub(super) fn execute(
    adapter: Arc<PluginUpstreamAdapter>,
    invocation: UpstreamAdapterInvocation,
) -> EventStream {
    let state = Execution {
        adapter,
        invocation,
        active: None,
        decoder: EventDecoder::default(),
        ended: false,
    };
    // try_unfold 首次 poll 才运行闭包；Core 在此前登记 attempt 并持有账号租约
    Box::pin(stream::try_unfold(state, |mut state| async move {
        if state.ended {
            return Ok(None);
        }
        let result = state.next().await;
        match result {
            Ok(event) => Ok(Some((event, state))),
            Err(error) => Err(state.invocation.account.record_failure(error).await),
        }
    }))
}

impl Execution {
    async fn next(&mut self) -> Result<gateway_core::event::ProviderEvent, ProviderError> {
        if self.active.is_none() {
            self.active = Some(self.begin().await?);
        }
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| invalid(UpstreamSendState::NotSent))?;
        let cancellation = self.invocation.context.cancellation();
        let chunk = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::new(ProviderErrorKind::Cancelled, active.managed.send_state.get())),
            result = active.stream.next() => result.map_err(|error| rpc_error(error, active.managed.send_state.get()))?,
        }.ok_or_else(|| invalid(active.managed.send_state.get()))?;
        let mut decoded = self.decoder.decode(
            &chunk,
            &self.adapter.declaration.protocol,
            self.adapter.declaration.transport.as_str(),
            self.invocation.account.as_ref(),
            active.managed.send_state.get(),
        )?;
        if self.decoder.completed {
            // 先确认 RPC 正常终结，再发布唯一 Completed；否则 Core 不会继续 poll 尾部错误
            let terminal = async {
                let tail = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(ProviderError::new(ProviderErrorKind::Cancelled, active.managed.send_state.get())),
                    result = active.stream.next() => result.map_err(|error| rpc_error(error, active.managed.send_state.get()))?,
                };
                if tail.is_some() {
                    return Err(invalid(active.managed.send_state.get()));
                }
                self.decoder.finish(active.managed.send_state.get())?;
                attach_continuation(&self.adapter, &self.invocation, active, &mut decoded).await
            }.await;
            if let Err(error) = terminal {
                // 尾部故障不得交付成功 wire，但已经解析的计量事实仍归 Core 结算
                let metering = decoded
                    .event
                    .canonical_facts()
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact,
                            gateway_core::event::GatewayEvent::Usage(_)
                                | gateway_core::event::GatewayEvent::CalculatedCost(_)
                        )
                    })
                    .cloned()
                    .map(gateway_core::event::ProviderEvent::canonical)
                    .collect();
                return Err(error.with_atomic_client_events(metering));
            }
            self.ended = true;
        }
        Ok(decoded.event)
    }

    async fn begin(&self) -> Result<ActiveCall, ProviderError> {
        let invalid = || invalid(UpstreamSendState::NotSent);
        let declaration = &self.adapter.declaration;
        let invocation = &self.invocation;
        if invocation.operation.protocol() != declaration.protocol
            || invocation.metadata.provider().as_str() != declaration.provider.as_str()
            || invocation.metadata.provider_account_id() != invocation.account.account_id()
            || !declaration
                .authentication_kinds
                .iter()
                .any(|kind| kind == invocation.account.authentication_kind())
        {
            return Err(invalid());
        }
        let timeout = invocation
            .context
            .deadline()
            .bounded(self.adapter.session.maximum_call_timeout());
        if timeout.is_zero() || invocation.context.cancellation().is_cancelled() {
            return Err(ProviderError::new(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ));
        }
        let mut context = self.adapter.session.context(Stage::Upstream, timeout);
        context.request_id = Some(invocation.context.request_id().as_str().to_owned());
        context.attempt_id = Some(format!(
            "{}:{}",
            invocation.context.request_id().as_str(),
            invocation.context.attempt_index()
        ));
        context.account_id = Some(invocation.account.account_id().as_str().to_owned());
        context.credential_revision = Some(invocation.account.credential_revision().get());
        let owner = ConnectionOwner {
            client_key_id: invocation.context.client_api_key_ref().as_str().to_owned(),
            account_id: invocation.account.account_id().as_str().to_owned(),
            credential_revision: invocation.account.credential_revision().get(),
        };
        let previous = previous_continuation(&self.adapter, invocation, &context, &owner)?;
        let managed = ManagedUpstream::new(
            Arc::clone(&invocation.account),
            Arc::clone(&self.adapter.target),
            invocation.context.execution_effects(),
            Arc::clone(&self.adapter.connections),
            owner,
            previous
                .as_ref()
                .and_then(|state| state.connection.as_deref()),
            declaration.transport == UpstreamTransport::WebSocket,
        )
        .map_err(|error| fault_error(error, UpstreamSendState::NotSent))?;
        let extension_scope = invocation
            .context
            .extension_scope()
            .extending(self.adapter.instance_id.clone())
            .ok_or_else(invalid)?;
        let scope = self
            .adapter
            .callbacks
            .prepare_upstream(&context, Arc::clone(&managed), extension_scope)
            .map_err(|error| fault_error(error, UpstreamSendState::NotSent))?;
        let request = UpstreamAdapterRequest {
            adapter_id: declaration.id.clone(),
            provider: declaration.provider,
            upstream_model: invocation
                .metadata
                .upstream_model()
                .ok_or_else(invalid)?
                .as_str()
                .to_owned(),
            client_key_id: invocation.context.client_api_key_ref().as_str().to_owned(),
            account_id: invocation.account.account_id().as_str().to_owned(),
            credential_revision: invocation.account.credential_revision().get(),
            protocol: invocation.operation.protocol().to_owned(),
            client_transport: invocation.context.client_transport().as_str().to_owned(),
            disable_fast: invocation.context.disable_fast(),
            headers: invocation
                .headers
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_vec()))
                .collect(),
            continuation: previous.map(|state| state.continuation),
        };
        let payload = request
            .encode(
                invocation
                    .operation
                    .middleware_body()
                    .map_err(|_| invalid())?
                    .to_vec(),
            )
            .map_err(|_| invalid())?;
        invocation.context.trace().record("plugin.upstream", serde_json::json!({ "instanceId": self.adapter.instance_id, "adapterId": declaration.id, "generation": context.generation, "transport": declaration.transport.as_str() }));
        let stream = tokio::select! {
            biased;
            () = invocation.context.cancellation().cancelled() => return Err(ProviderError::new(ProviderErrorKind::Cancelled, managed.send_state.get())),
            result = self.adapter.session.call_stream(EXECUTE_METHOD, context.clone(), serde_json::json!({}), payload) => result.map_err(|error| rpc_error(error, managed.send_state.get()))?,
        };
        if stream.initial.result != serde_json::json!({}) || !stream.initial.payload.is_empty() {
            return Err(super::event::invalid(managed.send_state.get()));
        }
        Ok(ActiveCall {
            context,
            stream,
            managed,
            _scope: scope,
        })
    }
}

fn previous_continuation(
    adapter: &PluginUpstreamAdapter,
    invocation: &UpstreamAdapterInvocation,
    context: &CallContext,
    owner: &ConnectionOwner,
) -> Result<Option<StoredContinuation>, ProviderError> {
    let invalid = || invalid(UpstreamSendState::NotSent);
    let Some(state) = invocation
        .operation
        .provider_session_state(adapter.declaration.provider.as_str())
    else {
        return if invocation.context.continuation().is_some() {
            Err(invalid())
        } else {
            Ok(None)
        };
    };
    let expected = extension_owner(
        adapter,
        context,
        state
            .extension_owner()
            .is_some_and(|owner| owner.connection_local),
    );
    if state.extension_owner() != Some(&expected) {
        return Err(invalid());
    }
    let stored: StoredContinuation =
        serde_json::from_value(serde_json::Value::Object(state.payload().clone()))
            .map_err(|_| invalid())?;
    if stored.client_key_id != owner.client_key_id
        || stored.account_id != owner.account_id
        || stored.credential_revision != owner.credential_revision
        || (stored.continuation.scope == ContinuationScope::ConnectionLocal)
            != stored.connection.is_some()
        || expected.connection_local != stored.connection.is_some()
    {
        return Err(invalid());
    }
    Ok(Some(stored))
}

async fn attach_continuation(
    adapter: &PluginUpstreamAdapter,
    invocation: &UpstreamAdapterInvocation,
    active: &ActiveCall,
    decoded: &mut DecodedEvent,
) -> Result<(), ProviderError> {
    let invalid = || invalid(active.managed.send_state.get());
    let Some(continuation) = decoded.continuation.take() else {
        if invocation
            .operation
            .provider_session_state(adapter.declaration.provider.as_str())
            .is_some()
        {
            return Err(invalid());
        }
        return Ok(());
    };
    if continuation.upstream_response_id.is_empty()
        || continuation.upstream_response_id.len() > 512
        || serde_json::to_vec(&continuation)
            .map_err(|_| invalid())?
            .len()
            > 32 * 1024
    {
        return Err(invalid());
    }
    let local = continuation.scope == ContinuationScope::ConnectionLocal;
    let connection = if local {
        Some(
            active
                .managed
                .retain_connection()
                .await
                .map_err(|error| fault_error(error, active.managed.send_state.get()))?,
        )
    } else {
        None
    };
    let state = StoredContinuation {
        continuation,
        connection,
        client_key_id: invocation.context.client_api_key_ref().as_str().to_owned(),
        account_id: invocation.account.account_id().as_str().to_owned(),
        credential_revision: invocation.account.credential_revision().get(),
    };
    let serde_json::Value::Object(payload) = serde_json::to_value(state).map_err(|_| invalid())?
    else {
        return Err(invalid());
    };
    let state = ProviderSessionState::new(adapter.declaration.provider.as_str(), payload)
        .map_err(|_| invalid())?
        .with_extension_owner(extension_owner(adapter, &active.context, local));
    decoded.event.attach_session_update(state);
    Ok(())
}

fn extension_owner(
    adapter: &PluginUpstreamAdapter,
    context: &CallContext,
    connection_local: bool,
) -> ExtensionSessionOwner {
    ExtensionSessionOwner {
        instance_id: adapter.instance_id.clone(),
        contribution_id: adapter.contribution_id.clone(),
        adapter_id: adapter.declaration.id.clone(),
        generation: context.generation,
        incarnation: context.incarnation.clone(),
        connection_local,
    }
}

fn rpc_error(error: RpcError, sent: UpstreamSendState) -> ProviderError {
    match error {
        RpcError::Remote(fault) => fault_error(fault, sent),
        RpcError::Timeout => ProviderError::new(ProviderErrorKind::Timeout, sent),
        RpcError::Cancelled => ProviderError::new(ProviderErrorKind::Cancelled, sent),
        _ => ProviderError::new(ProviderErrorKind::Unavailable, sent),
    }
}

fn fault_error(fault: PluginFault, sent: UpstreamSendState) -> ProviderError {
    let kind = match fault.code {
        ErrorCode::Timeout => ProviderErrorKind::Timeout,
        ErrorCode::Cancelled => ProviderErrorKind::Cancelled,
        ErrorCode::Unsupported => ProviderErrorKind::Unsupported,
        ErrorCode::InvalidInput | ErrorCode::Rejected | ErrorCode::PermissionDenied => {
            ProviderErrorKind::InvalidRequest
        }
        _ => ProviderErrorKind::Unavailable,
    };
    ProviderError::new(kind, sent)
}
