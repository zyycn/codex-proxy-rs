use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
pub(super) struct OAuthRecoveryLogCapture(Arc<Mutex<Vec<u8>>>);

static OAUTH_RECOVERY_LOG_CAPTURE_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

pub(super) struct OAuthRecoveryLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for OAuthRecoveryLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("OAuth recovery log buffer lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for OAuthRecoveryLogCapture {
    type Writer = OAuthRecoveryLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        OAuthRecoveryLogWriter(Arc::clone(&self.0))
    }
}

impl OAuthRecoveryLogCapture {
    pub(super) fn dispatch(&self) -> tracing::Dispatch {
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_target(true)
            .without_time()
            .with_writer(self.clone())
            .finish();
        tracing::Dispatch::new(subscriber)
    }

    pub(super) fn recovery_record(&self) -> Value {
        let bytes = self
            .0
            .lock()
            .expect("OAuth recovery log buffer lock")
            .clone();
        std::str::from_utf8(&bytes)
            .expect("JSON log output is UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON log record"))
            .find(|record: &Value| {
                record["target"] == "openai_oauth_recovery"
                    && record["fields"]["event"] == "openai_oauth_recovery"
            })
            .expect("OAuth recovery log record")
    }
}

pub(super) async fn lock_oauth_recovery_log_capture() -> MutexGuard<'static, ()> {
    OAUTH_RECOVERY_LOG_CAPTURE_LOCK.lock().await
}

mod admin;
mod agent_identity;
mod catalog;
mod contract;
mod cookie;
mod identity;
mod oauth;
mod quota;
mod refresh;
mod token_client;
mod types;
