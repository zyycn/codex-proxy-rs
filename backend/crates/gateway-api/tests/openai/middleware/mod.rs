//! OpenAI 请求与响应中间件的改写、短路及交付行为测试

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::{
    engine::middleware::{
        FrozenMiddlewarePlan, MiddlewareBody, MiddlewareContext, MiddlewareError, MiddlewareFrame,
        MiddlewareFraming, MiddlewareHeader, MiddlewareMount, MiddlewareNext, MiddlewarePlan,
        MiddlewareRequest, MiddlewareResponse,
    },
    operation::OperationKind,
    runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};

/// OpenAI API 入口共用的中间件夹具；授权和 RPC 由 Runtime 行为测试覆盖
#[derive(Debug, Default)]
pub(super) struct RequestMiddleware {
    pub(super) endpoints: Mutex<Vec<String>>,
    pub(super) live_leases: Arc<AtomicUsize>,
    pub(super) expected_operation: Option<OperationKind>,
    pub(super) committed_statuses: Option<Arc<Mutex<Vec<u16>>>>,
    pub(super) response_actions: Mutex<VecDeque<ResponseFrameAction>>,
    pub(super) response_headers: Vec<MiddlewareHeader>,
    pub(super) short_circuit_json: Option<Bytes>,
    pub(super) settings: Option<fn(&mut gateway_core::settings::ExecutionSettings)>,
}

#[derive(Debug, Clone)]
pub(super) enum ResponseFrameAction {
    Continue,
    Replace(Bytes),
    Drop,
}

impl RequestMiddleware {
    pub(super) fn frozen(self: &Arc<Self>) -> FrozenMiddlewarePlan {
        self.live_leases.fetch_add(1, Ordering::SeqCst);
        FrozenMiddlewarePlan::new(
            self.clone(),
            ExtensionSetReference::new(
                ExtensionSetId::new("request-middleware".to_owned()).unwrap(),
                Arc::new(RequestLease(self.live_leases.clone())),
            ),
        )
    }

    #[must_use]
    pub(super) fn with_response_actions(self, actions: Vec<ResponseFrameAction>) -> Self {
        Self {
            response_actions: Mutex::new(actions.into()),
            ..self
        }
    }

    #[must_use]
    pub(super) fn with_response_headers(self, headers: Vec<MiddlewareHeader>) -> Self {
        Self {
            response_headers: headers,
            ..self
        }
    }

    #[must_use]
    pub(super) fn with_json_short_circuit(self, body: Bytes) -> Self {
        Self {
            short_circuit_json: Some(body),
            ..self
        }
    }
}

struct RequestLease(Arc<AtomicUsize>);

impl ExtensionSetLease for RequestLease {
    fn is_ready(&self) -> bool {
        true
    }
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl MiddlewarePlan for RequestMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        mut request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Request);
        assert_eq!(context.operation(), self.expected_operation);
        assert!(context.attempt_index().is_none());
        assert!(context.account_id().is_none());
        self.endpoints
            .lock()
            .unwrap()
            .push(context.endpoint().to_owned());
        let statuses = self.committed_statuses.clone();
        let actions = std::mem::take(&mut *self.response_actions.lock().unwrap());
        let response_headers = self.response_headers.clone();
        let short_circuit_json = self.short_circuit_json.clone();
        if let Some(update) = self.settings {
            let settings = request.settings().expect("effective request settings");
            let mut values = settings.execution_values().expect("execution scope");
            update(&mut values);
            let updated = settings
                .replace_execution(&values, "test-middleware")
                .map_err(|_| MiddlewareError::InvalidState);
            request = match updated {
                Ok(settings) => request.with_settings(settings),
                Err(error) => return Box::pin(async move { Err(error) }),
            };
        }
        Box::pin(async move {
            if let Some(body) = short_circuit_json {
                return Ok(MiddlewareResponse::new(
                    "openai".to_owned(),
                    200,
                    Vec::new(),
                    Box::new(SingleFrameBody(Some(MiddlewareFrame::new(
                        body,
                        MiddlewareFraming::JsonDocument,
                        true,
                    )))),
                ));
            }
            let count = statuses.as_ref().map(|values| values.lock().unwrap().len());
            let response = next.run(request).await?;
            assert_eq!(
                statuses.as_ref().map(|values| values.lock().unwrap().len()),
                count
            );
            let (protocol, status, mut headers, body, envelope) = response.into_parts();
            headers.push(MiddlewareHeader::new(
                "x-request-middleware",
                Bytes::from_static(b"applied"),
            ));
            headers.extend(response_headers);
            let mut response = MiddlewareResponse::new(
                protocol,
                status,
                headers,
                Box::new(ResponseMiddlewareBody {
                    body,
                    actions,
                    transformed_pending: false,
                }),
            );
            if let Some(envelope) = envelope {
                response = response.with_envelope(envelope);
            }
            Ok(response)
        })
    }
}

struct SingleFrameBody(Option<MiddlewareFrame>);

impl MiddlewareBody for SingleFrameBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async { Ok(self.0.take()) })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { drop(self) })
    }
}

struct ResponseMiddlewareBody {
    body: Box<dyn MiddlewareBody>,
    actions: VecDeque<ResponseFrameAction>,
    transformed_pending: bool,
}

impl MiddlewareBody for ResponseMiddlewareBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            loop {
                let Some(frame) = self.body.next_frame().await? else {
                    return Ok(None);
                };
                let action = self
                    .actions
                    .pop_front()
                    .unwrap_or(ResponseFrameAction::Continue);
                let transformed = self.transformed_pending || frame.transformed();
                let (bytes, framing, terminal, envelope) = frame.into_parts();
                match action {
                    ResponseFrameAction::Continue => {
                        self.transformed_pending = false;
                        let mut frame = MiddlewareFrame::new(bytes, framing, terminal)
                            .with_transformed(transformed);
                        if let Some(envelope) = envelope {
                            frame = frame.with_envelope(envelope);
                        }
                        return Ok(Some(frame));
                    }
                    ResponseFrameAction::Replace(bytes) => {
                        self.transformed_pending = false;
                        let mut frame =
                            MiddlewareFrame::new(bytes, framing, terminal).with_transformed(true);
                        if let Some(envelope) = envelope {
                            frame = frame.with_envelope(envelope);
                        }
                        return Ok(Some(frame));
                    }
                    ResponseFrameAction::Drop if envelope.is_some() => {
                        return Err(MiddlewareError::InvalidState);
                    }
                    ResponseFrameAction::Drop => self.transformed_pending = true,
                }
            }
        })
    }

    fn commit_downstream(
        &mut self,
        status: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.body.commit_downstream(status)
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.body.record_client_status(status)
    }

    fn is_finalized(&self) -> bool {
        self.body.is_finalized()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        let Self { body, .. } = *self;
        body.close()
    }
}
