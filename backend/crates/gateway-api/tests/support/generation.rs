//! 各生成协议共用的执行会话夹具，记录提交、取消与终结事实

use futures::future::BoxFuture;
use gateway_core::{
    engine::{
        CommitRequirement, CoordinatedEvent, EngineError,
        execution::{ExecutionSession, ResponseCommit},
    },
    event::{ProviderEvent, ProviderResponseHeader},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
pub(crate) struct Trace {
    pub commits: AtomicUsize,
    pub discarded: AtomicUsize,
    pub cancelled: AtomicBool,
    pub finalized: AtomicBool,
    pub detached: AtomicUsize,
    pub response_commit: ResponseCommit,
}

#[derive(Default)]
pub(crate) enum Finish {
    #[default]
    Success,
    Error,
    Pending,
}

pub(crate) struct Session {
    events: VecDeque<Vec<ProviderEvent>>,
    pub trace: Arc<Trace>,
    pub finish: Finish,
    pub delay: Option<Duration>,
    pub fail_commit: bool,
    pub response_headers: Vec<ProviderResponseHeader>,
    committed: bool,
    buffered: bool,
}

impl Session {
    pub fn new(events: Vec<ProviderEvent>) -> Self {
        Self {
            events: events.into_iter().map(|event| vec![event]).collect(),
            trace: Arc::default(),
            finish: Finish::Success,
            delay: None,
            fail_commit: false,
            response_headers: Vec::new(),
            committed: false,
            buffered: false,
        }
    }

    pub fn with_batch(mut self, events: Vec<ProviderEvent>) -> Self {
        self.events.push_back(events);
        self
    }
}

impl ExecutionSession for Session {
    fn response_commit(&self) -> ResponseCommit {
        self.trace.response_commit.clone()
    }

    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            if let Some(delay) = self.delay.take() {
                tokio::time::sleep(delay).await;
            }
            if let Some(event) = self.events.pop_front() {
                let requirement = if self.committed {
                    CommitRequirement::AlreadyCommitted
                } else {
                    CommitRequirement::CommitBeforeDelivery
                };
                return CoordinatedEvent::try_batch(event, requirement).map(Some);
            }
            match self.finish {
                Finish::Success => {
                    self.trace.finalized.store(true, Ordering::SeqCst);
                    Ok(None)
                }
                Finish::Error => {
                    self.trace.finalized.store(true, Ordering::SeqCst);
                    Err(EngineError::ProviderMetadataMismatch)
                }
                Finish::Pending => futures::future::pending().await,
            }
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        self.buffered = true;
        Box::pin(async move { Ok(self.events.drain(..).flatten().collect()) })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        &self.response_headers
    }

    fn discard_pending_delivery(&mut self) -> Result<(), EngineError> {
        self.trace.discarded.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn commit_downstream(&mut self, status: Option<u16>) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.commits.fetch_add(1, Ordering::SeqCst);
            if self.fail_commit {
                return Err(EngineError::ProviderMetadataMismatch);
            }
            self.trace.response_commit.record(status);
            self.committed = true;
            if self.buffered {
                self.trace.finalized.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.trace.response_commit.record(Some(status));
            Ok(())
        })
    }

    fn is_finalized(&self) -> bool {
        self.trace.finalized.load(Ordering::SeqCst)
    }
    fn cancel(&self) {
        self.trace.cancelled.store(true, Ordering::SeqCst);
    }
    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            self.trace.detached.fetch_add(1, Ordering::SeqCst);
            self.trace.finalized.store(true, Ordering::SeqCst);
        })
    }
}
