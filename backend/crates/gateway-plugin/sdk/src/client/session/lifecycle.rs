//! 调用取消、期限等待与插件生命周期钩子的分派

use std::sync::Arc;

use tokio::sync::watch;

use crate::CallContext;

use super::{PluginHandler, SessionError};

/// 一次宿主调用的可克隆取消信号
#[derive(Clone)]
pub struct CallCancellation {
    receiver: watch::Receiver<bool>,
}

impl CallCancellation {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        if *receiver.borrow() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }
}

pub(super) struct CancellationSource {
    sender: watch::Sender<bool>,
}

impl CancellationSource {
    pub(super) fn new() -> (Self, CallCancellation) {
        let (sender, receiver) = watch::channel(false);
        (Self { sender }, CallCancellation { receiver })
    }

    pub(super) fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

enum LifecycleEvent {
    Cancel(CallContext),
    Quiesce,
    Shutdown,
}

#[derive(Clone)]
pub(super) struct LifecycleHooks {
    events: std::sync::mpsc::SyncSender<LifecycleEvent>,
}

impl LifecycleHooks {
    pub(super) fn new(
        handler: Arc<dyn PluginHandler>,
        capacity: usize,
    ) -> Result<Self, SessionError> {
        let (events, received) = std::sync::mpsc::sync_channel(capacity);
        std::thread::Builder::new()
            .name("gateway-plugin-sdk-lifecycle".into())
            .spawn(move || {
                while let Ok(event) = received.recv() {
                    match event {
                        LifecycleEvent::Cancel(context) => handler.cancel(&context),
                        LifecycleEvent::Quiesce => handler.quiesce(),
                        LifecycleEvent::Shutdown => {
                            handler.shutdown();
                            break;
                        }
                    }
                }
            })
            .map_err(|_| SessionError::Closed)?;
        Ok(Self { events })
    }

    pub(super) fn cancel(&self, context: CallContext) {
        let _ = self.events.try_send(LifecycleEvent::Cancel(context));
    }

    pub(super) fn quiesce(&self) {
        let _ = self.events.try_send(LifecycleEvent::Quiesce);
    }

    pub(super) fn shutdown(&self) {
        let _ = self.events.try_send(LifecycleEvent::Shutdown);
    }
}
