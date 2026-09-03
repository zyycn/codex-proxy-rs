use std::sync::{Arc, Mutex, atomic::AtomicBool, atomic::Ordering};

use bytes::Bytes;
use futures::future::BoxFuture;
use gateway_core::engine::execution::ExecutionSession;
use gateway_core::engine::{CoordinatedEvent, EngineError};
use gateway_core::error::ProviderError;
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};

pub(super) struct BufferedJsonSession {
    body: Option<Bytes>,
    response_headers: Vec<ProviderResponseHeader>,
    response_status: Option<u16>,
    committed_statuses: Arc<Mutex<Vec<u16>>>,
    finalized: AtomicBool,
    failure: Option<ProviderError>,
}

impl BufferedJsonSession {
    pub(super) fn success(
        body: Bytes,
        response_status: u16,
        response_headers: Vec<ProviderResponseHeader>,
        committed_statuses: Arc<Mutex<Vec<u16>>>,
    ) -> Self {
        Self {
            body: Some(body),
            response_headers,
            response_status: Some(response_status),
            committed_statuses,
            finalized: AtomicBool::new(false),
            failure: None,
        }
    }

    pub(super) fn failure(error: ProviderError) -> Self {
        Self {
            body: None,
            response_headers: Vec::new(),
            response_status: None,
            committed_statuses: Arc::new(Mutex::new(Vec::new())),
            finalized: AtomicBool::new(false),
            failure: Some(error),
        }
    }
}

impl ExecutionSession for BufferedJsonSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async { unreachable!("buffered JSON delivery does not stream") })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            if let Some(error) = self.failure.take() {
                return Err(EngineError::Provider(error));
            }
            Ok(vec![ProviderEvent::wire(
                ProtocolWireEvent::raw_json(
                    "openai",
                    self.body.take().expect("single buffered JSON response"),
                )
                .expect("OpenAI protocol"),
            )])
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.response_headers
    }

    fn response_status_code(&self) -> Option<u16> {
        self.response_status
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.committed_statuses
                .lock()
                .expect("committed status lock")
                .push(client_status_code.expect("buffered JSON response status"));
            self.finalized.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn record_client_status(&mut self, _: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async { Ok(()) })
    }

    fn is_finalized(&self) -> bool {
        self.finalized.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.finalized.store(true, Ordering::Release);
    }

    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.finalized.store(true, Ordering::Release);
        })
    }
}
