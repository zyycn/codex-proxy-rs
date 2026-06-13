use thiserror::Error;
use tokio::task::JoinHandle;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler already destroyed")]
    Destroyed,
    #[error("account {0} not found")]
    AccountNotFound(String),
    #[error("refresh lock acquisition failed")]
    LockFailed,
}

pub enum SchedulerHandle {
    Channel(tokio::sync::mpsc::Sender<()>),
    JoinHandle(JoinHandle<()>),
}

impl SchedulerHandle {
    pub fn new(shutdown_tx: tokio::sync::mpsc::Sender<()>) -> Self {
        Self::Channel(shutdown_tx)
    }

    pub fn from_join_handle(handle: JoinHandle<()>) -> Self {
        Self::JoinHandle(handle)
    }

    pub async fn shutdown(self) {
        match self {
            Self::Channel(shutdown_tx) => {
                let _ = shutdown_tx.send(()).await;
            }
            Self::JoinHandle(handle) => {
                handle.abort();
            }
        }
    }
}
