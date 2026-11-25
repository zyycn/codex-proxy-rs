//! 握手消费当前 HTTP 续体；收发继续使用同一调用的受管消息资源

use super::*;
use crate::callback::websocket_middleware::Invocation as Messages;
use gateway_core::engine::middleware::{http::upgrade, websocket as ws};
use gateway_plugin_sdk::call::middleware::websocket as message;
use gateway_plugin_sdk::call::middleware::{BODY_CLOSE_METHOD, BODY_READ_METHOD};

#[derive(Clone)]
pub(super) struct Session {
    pending: upgrade::PendingSession,
    status: http::StatusCode,
    messages: Arc<Messages>,
}
struct Sender(upgrade::PendingSession);
impl ws::Sender for Sender {
    fn send(&self, message: ws::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            self.0
                .clone()
                .await
                .map_err(|error| MiddlewareError::Remote {
                    rejected: error.is_rejected(),
                    source: Box::new(error),
                })?
                .send(message)
                .await
        })
    }
}

impl Invocation {
    pub(super) async fn upgrade(&self, upgrade: wire::Upgrade) -> Result<RpcReply, PluginFault> {
        let request = upgrade.request;
        let method = request.method.parse().map_err(|_| invalid())?;
        let uri = request.uri.parse().map_err(|_| invalid())?;
        let headers = headers(request.headers)?;
        let settings = super::resolve_settings(
            self.request_settings(),
            request.settings,
            request.timeout_ms,
            &self.instance_id,
        )?;
        let mut parts = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.next.take().ok_or_else(unavailable)?;
            state.settings = settings.runtime.clone();
            state.request.take().ok_or_else(unavailable)?
        };
        let transport = parts
            .extensions
            .remove::<Arc<dyn upgrade::WebSocketUpgrade>>()
            .ok_or_else(unavailable)?;
        parts.method = method;
        parts.uri = uri;
        parts.version = version(request.version);
        parts.headers = headers;
        parts.extensions.insert(settings);
        let (response, pending) = transport
            .accept(parts, upgrade.protocols)
            .await
            .map_err(|_| invalid())?;
        let status = response.status();
        let reply = self.resources.response(response)?;
        *self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Session {
            status,
            messages: Messages::session(Arc::new(Sender(pending.clone()))),
            pending,
        });
        Ok(reply)
    }

    pub(crate) async fn session_ready(&self) -> Result<(), MiddlewareError> {
        let pending = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .ok_or(MiddlewareError::InvalidState)?
            .pending
            .clone();
        pending.await.map_err(|error| MiddlewareError::Remote {
            rejected: error.is_rejected(),
            source: Box::new(error),
        })?;
        Ok(())
    }

    pub(crate) fn session_status(&self) -> Option<http::StatusCode> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|session| session.status)
    }

    pub(super) fn is_session_method(&self, method: &str, params: &serde_json::Value) -> bool {
        if matches!(method, wire::RECEIVE_METHOD | message::SEND_METHOD) {
            return true;
        }
        matches!(method, BODY_READ_METHOD | BODY_CLOSE_METHOD)
            && params
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|handle| {
                    self.session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .is_some_and(|session| session.messages.owns_payload(handle))
                })
    }

    pub(super) async fn session_call(
        &self,
        method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
        maximum: usize,
    ) -> Result<RpcReply, PluginFault> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(unavailable)?;
        if method == wire::RECEIVE_METHOD {
            if params != serde_json::json!({}) || !payload.is_empty() {
                return Err(invalid());
            }
            let socket = session
                .pending
                .await
                .map_err(|error| crate::callback::error::middleware(&error))?;
            let message = socket
                .receive()
                .await
                .map_err(|error| crate::callback::error::middleware(&error))?;
            Ok(RpcReply {
                result: serde_json::to_value(
                    message
                        .map(|message| session.messages.encode(message))
                        .transpose()?,
                )
                .map_err(|_| invalid())?,
                payload: Vec::new(),
            })
        } else {
            session
                .messages
                .invoke(method, params, payload, maximum)
                .await
        }
    }
}
