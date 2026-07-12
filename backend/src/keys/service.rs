//! 客户端 API Key 鉴权服务。

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::time::sleep;

use super::store::{PgClientKeyStore, PgClientKeyStoreError};

const CLIENT_KEY_LAST_USED_FLUSH_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum ClientKeyStoreError {
    #[error("client key store operation failed: {message}")]
    OperationFailed { message: String },
}

pub type ClientKeyStoreResult<T> = Result<T, ClientKeyStoreError>;

/// 客户端 API Key 验证服务。鉴权始终走 PostgreSQL 唯一索引点查。
#[derive(Clone)]
pub struct KeyVerifier {
    store: PgClientKeyStore,
    pending_last_used: Arc<Mutex<BTreeMap<String, DateTime<Utc>>>>,
    flush_scheduled: Arc<AtomicBool>,
    flush_delay: Duration,
}

impl KeyVerifier {
    pub fn new(store: PgClientKeyStore) -> Self {
        Self {
            store,
            pending_last_used: Arc::new(Mutex::new(BTreeMap::new())),
            flush_scheduled: Arc::new(AtomicBool::new(false)),
            flush_delay: CLIENT_KEY_LAST_USED_FLUSH_DELAY,
        }
    }

    /// 验证明文 key；成功时返回稳定 key ID，供请求事实归因。
    pub async fn verify(&self, plaintext: &str) -> ClientKeyStoreResult<Option<String>> {
        if !plaintext.starts_with("sk_") {
            return Ok(None);
        }
        let key_id = self
            .store
            .find_enabled_id_by_key(plaintext)
            .await
            .map_err(map_store_error)?;
        if let Some(key_id) = &key_id {
            self.queue_last_used_touch(key_id.clone());
        }
        Ok(key_id)
    }

    fn queue_last_used_touch(&self, id: String) {
        self.pending_last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, Utc::now());
        self.schedule_flush();
    }

    fn schedule_flush(&self) {
        if self
            .flush_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            sleep(service.flush_delay).await;
            service.flush_pending_last_used().await;
        });
    }

    pub async fn flush_pending_last_used(&self) {
        let updates = self.take_pending_last_used();
        if let Err(error) = self.store.touch_last_used_batch(&updates).await {
            tracing::error!(error = %error, "failed to flush client key last_used_at batch");
            merge_last_used_updates(
                &mut self
                    .pending_last_used
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                updates,
            );
        }
        self.flush_scheduled.store(false, Ordering::Release);
        if !self
            .pending_last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
        {
            self.schedule_flush();
        }
    }

    fn take_pending_last_used(&self) -> BTreeMap<String, DateTime<Utc>> {
        std::mem::take(
            &mut *self
                .pending_last_used
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

fn merge_last_used_updates(
    pending: &mut BTreeMap<String, DateTime<Utc>>,
    updates: BTreeMap<String, DateTime<Utc>>,
) {
    for (id, updated_at) in updates {
        pending
            .entry(id)
            .and_modify(|pending_at| {
                if updated_at > *pending_at {
                    *pending_at = updated_at;
                }
            })
            .or_insert(updated_at);
    }
}

fn map_store_error(error: PgClientKeyStoreError) -> ClientKeyStoreError {
    ClientKeyStoreError::OperationFailed {
        message: error.to_string(),
    }
}
