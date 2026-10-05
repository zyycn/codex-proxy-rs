//! 插件中间件正文句柄的读取、交付事实与关闭生命周期

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use bytes::Bytes;
use gateway_core::engine::middleware::{
    MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareFrameEnvelope, MiddlewareFraming,
};
use gateway_plugin_sdk::{ErrorCode, PluginFault, call::middleware::MiddlewareBodyDisposition};

use super::{invalid, resource_unavailable};

pub(super) struct BodyResource {
    expected_framing: MiddlewareFraming,
    downstream_error: Arc<Mutex<Option<MiddlewareError>>>,
    finalized: AtomicBool,
    state: tokio::sync::Mutex<BodyState>,
    source_released: tokio::sync::Notify,
}

struct BodyState {
    body: Option<Box<dyn MiddlewareBody>>,
    read_started: bool,
    next_source_id: u64,
    source: Option<(u64, SourceFrame)>,
}

struct SourceFrame {
    bytes: Bytes,
    framing: MiddlewareFraming,
    terminal: bool,
    transformed: bool,
    envelope: Option<MiddlewareFrameEnvelope>,
}

pub(super) struct BodyReadFrame {
    pub(super) bytes: Bytes,
    pub(super) framing: MiddlewareFraming,
    pub(super) source_id: u64,
    pub(super) terminal: bool,
}

#[derive(Clone)]
pub(crate) struct MiddlewareBodyAuthority(Arc<BodyResource>);

impl MiddlewareBodyAuthority {
    pub(super) fn new(body: Arc<BodyResource>) -> Self {
        Self(body)
    }

    pub(crate) async fn resolve_source_output(
        &self,
        source_id: u64,
        disposition: MiddlewareBodyDisposition,
        bytes: Bytes,
    ) -> Result<Option<MiddlewareFrame>, MiddlewareError> {
        let mut state = self.0.state.lock().await;
        let finishes_source = matches!(
            disposition,
            MiddlewareBodyDisposition::Only
                | MiddlewareBodyDisposition::Last
                | MiddlewareBodyDisposition::Drop
        );
        let (framing, terminal, source_bytes, transformed, envelope) = if finishes_source {
            let (stored_id, source) = state.source.take().ok_or(MiddlewareError::InvalidState)?;
            if stored_id != source_id {
                return Err(MiddlewareError::InvalidState);
            }
            self.0.source_released.notify_waiters();
            (
                source.framing,
                source.terminal,
                source.bytes,
                source.transformed,
                source.envelope,
            )
        } else {
            let (stored_id, source) = state.source.as_ref().ok_or(MiddlewareError::InvalidState)?;
            if *stored_id != source_id {
                return Err(MiddlewareError::InvalidState);
            }
            (
                source.framing,
                source.terminal,
                source.bytes.clone(),
                source.transformed,
                None,
            )
        };
        if disposition == MiddlewareBodyDisposition::Drop && terminal {
            return Err(MiddlewareError::InvalidState);
        }
        let framing = if framing == MiddlewareFraming::RawBytes
            && source_bytes.is_empty()
            && !bytes.is_empty()
        {
            self.0.expected_framing
        } else {
            framing
        };
        let output_transformed =
            transformed || disposition != MiddlewareBodyDisposition::Only || bytes != source_bytes;
        let mut frame = MiddlewareFrame::new(bytes, framing, finishes_source && terminal)
            .with_transformed(output_transformed);
        let has_envelope = envelope.is_some();
        if let Some(envelope) = envelope {
            frame = frame.with_envelope(envelope);
        }
        if disposition == MiddlewareBodyDisposition::Drop && !has_envelope {
            Ok(None)
        } else {
            Ok(Some(frame))
        }
    }

    pub(crate) async fn has_pending_sources(&self) -> bool {
        self.0.state.lock().await.source.is_some()
    }

    pub(crate) async fn commit_downstream(
        &self,
        client_status_code: Option<u16>,
    ) -> Result<(), MiddlewareError> {
        let mut state = self.0.state.lock().await;
        let body = state.body.as_mut().ok_or(MiddlewareError::InvalidState)?;
        let result = body.commit_downstream(client_status_code).await;
        self.0
            .finalized
            .store(body.is_finalized(), Ordering::Release);
        result
    }

    pub(crate) async fn record_client_status(
        &self,
        client_status_code: u16,
    ) -> Result<(), MiddlewareError> {
        let mut state = self.0.state.lock().await;
        let body = state.body.as_mut().ok_or(MiddlewareError::InvalidState)?;
        let result = body.record_client_status(client_status_code).await;
        self.0
            .finalized
            .store(body.is_finalized(), Ordering::Release);
        result
    }

    #[must_use]
    pub(crate) fn is_finalized(&self) -> bool {
        self.0.finalized.load(Ordering::Acquire)
    }
}

impl BodyResource {
    pub(super) fn new(
        expected_framing: MiddlewareFraming,
        downstream_error: Arc<Mutex<Option<MiddlewareError>>>,
        body: Box<dyn MiddlewareBody>,
    ) -> Arc<Self> {
        Arc::new(Self {
            expected_framing,
            downstream_error,
            finalized: AtomicBool::new(body.is_finalized()),
            state: tokio::sync::Mutex::new(BodyState {
                body: Some(body),
                read_started: false,
                next_source_id: 0,
                source: None,
            }),
            source_released: tokio::sync::Notify::new(),
        })
    }

    pub(super) const fn expected_framing(&self) -> MiddlewareFraming {
        self.expected_framing
    }

    pub(super) async fn read(&self, maximum: usize) -> Result<Option<BodyReadFrame>, PluginFault> {
        // SDK 发出上一源的映射帧后即可请求下一源；消费者可能尚未处理排队帧
        // 等待归还而非报冲突，仍只保留一个源；父 RPC 的期限与取消约束等待
        let mut state = loop {
            let released = self.source_released.notified();
            let state = self.state.lock().await;
            if state.source.is_none() {
                break state;
            }
            drop(state);
            released.await;
        };
        state.read_started = true;
        let body = state.body.as_mut().ok_or_else(resource_unavailable)?;
        let frame = match body.next_frame().await {
            Ok(frame) => frame,
            Err(error) => {
                let mut stored = self
                    .downstream_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if stored.is_none() {
                    *stored = Some(error);
                }
                return Err(PluginFault::new(
                    ErrorCode::Fault,
                    "middleware downstream body failed",
                ));
            }
        };
        self.finalized.store(body.is_finalized(), Ordering::Release);
        let Some(frame) = frame else {
            return Ok(None);
        };
        let transformed = frame.transformed();
        let (bytes, framing, terminal, envelope) = frame.into_parts();
        // canonical-only 事件没有客户端正文，仍须保留并允许读取其完整事实
        let facts_only =
            framing == MiddlewareFraming::RawBytes && bytes.is_empty() && envelope.is_some();
        if (!facts_only && framing != self.expected_framing) || bytes.len() > maximum {
            return Err(invalid());
        }
        let source_id = state.next_source_id.checked_add(1).ok_or_else(invalid)?;
        state.next_source_id = source_id;
        state.source = Some((
            source_id,
            SourceFrame {
                bytes: bytes.clone(),
                framing,
                terminal,
                transformed,
                envelope,
            },
        ));
        Ok(Some(BodyReadFrame {
            bytes,
            framing: self.expected_framing,
            source_id,
            terminal,
        }))
    }

    pub(super) async fn facts(&self, source_id: u64) -> Result<Option<Vec<u8>>, PluginFault> {
        let state = self.state.lock().await;
        let (stored_id, source) = state.source.as_ref().ok_or_else(resource_unavailable)?;
        if *stored_id != source_id {
            return Err(invalid());
        }
        source
            .envelope
            .as_ref()
            .map(|envelope| {
                super::super::facts::snapshot(envelope.event())?
                    .encode()
                    .map_err(|_| {
                        PluginFault::new(
                            ErrorCode::Capacity,
                            "middleware facts exceed the payload limit",
                        )
                    })
            })
            .transpose()
    }

    pub(super) async fn take_unread(&self) -> Option<Box<dyn MiddlewareBody>> {
        let mut state = self.state.lock().await;
        if state.read_started {
            return None;
        }
        state.body.take()
    }

    pub(super) async fn close(&self) {
        let body = {
            let mut state = self.state.lock().await;
            state.source.take();
            self.source_released.notify_waiters();
            state.body.take()
        };
        if let Some(body) = body {
            body.close().await;
        }
    }

    pub(super) fn close_detached(self: Arc<Self>) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { self.close().await });
        }
    }
}
