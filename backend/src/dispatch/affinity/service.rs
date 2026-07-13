//! 运行时会话亲和服务。

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use super::{
    store::{RedisSessionAffinityStore, RedisSessionAffinityStoreError},
    types::{CyberPolicyFailureSnapshot, CyberPolicySessionState, SessionAffinityEntry},
};

const DEFAULT_SESSION_AFFINITY_TTL_SECS: i64 = 4 * 60 * 60;

/// 运行时会话亲和性服务。
#[derive(Clone)]
pub struct SessionAffinityService {
    store: RedisSessionAffinityStore,
    ttl: Duration,
}

/// 运行时会话亲和性错误。
#[derive(Debug, Error)]
pub enum SessionAffinityError {
    #[error("session affinity store error: {0}")]
    Store(#[from] RedisSessionAffinityStoreError),
}

impl SessionAffinityService {
    pub fn new(store: RedisSessionAffinityStore) -> Self {
        let ttl = Duration::seconds(DEFAULT_SESSION_AFFINITY_TTL_SECS);
        Self { store, ttl }
    }

    pub async fn record(
        &self,
        response_id: String,
        entry: SessionAffinityEntry,
    ) -> Result<(), SessionAffinityError> {
        Ok(self.store.upsert(&response_id, &entry, self.ttl).await?)
    }

    pub async fn lookup(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<SessionAffinityEntry> {
        self.entry(response_id, now).await
    }

    pub async fn lookup_latest_account_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.latest(conversation_id, max_age, variant_hash, now)
            .await
            .map(|(_, entry)| entry.account_id)
    }

    pub async fn forget(&self, response_id: &str) -> bool {
        match self.store.forget(response_id).await {
            Ok(forgotten) => forgotten,
            Err(error) => {
                tracing::warn!(
                    response_id,
                    error = %error,
                    "Failed to remove Redis session affinity"
                );
                false
            }
        }
    }

    pub async fn forget_account(&self, account_id: &str) -> bool {
        match self.store.forget_account(account_id).await {
            Ok(forgotten) => forgotten > 0,
            Err(error) => {
                tracing::warn!(account_id, error = %error, "Failed to remove account affinities");
                false
            }
        }
    }

    pub(crate) async fn load_cyber_policy_state(
        &self,
        session_key: &str,
    ) -> Result<Option<CyberPolicySessionState>, SessionAffinityError> {
        Ok(self.store.cyber_policy_state(session_key).await?)
    }

    pub(crate) async fn persist_cyber_policy_failure(
        &self,
        session_key: &str,
        failure: &CyberPolicyFailureSnapshot,
        max_accounts: usize,
        ttl: Duration,
    ) -> Result<CyberPolicySessionState, SessionAffinityError> {
        Ok(self
            .store
            .record_cyber_policy_failure(session_key, failure, max_accounts, ttl)
            .await?)
    }

    pub(crate) async fn delete_cyber_policy_state(
        &self,
        session_key: &str,
        expected_revision: &str,
    ) -> Result<bool, SessionAffinityError> {
        Ok(self
            .store
            .clear_cyber_policy_state(session_key, expected_revision)
            .await?)
    }

    async fn entry(&self, response_id: &str, now: DateTime<Utc>) -> Option<SessionAffinityEntry> {
        match self.store.get(response_id, now, self.ttl).await {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(response_id, error = %error, "Failed to read Redis session affinity");
                None
            }
        }
    }

    async fn latest(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<(String, SessionAffinityEntry)> {
        match self
            .store
            .latest_by_conversation(conversation_id, max_age, variant_hash, now, self.ttl)
            .await
        {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(conversation_id, error = %error, "Failed to query Redis affinity index");
                None
            }
        }
    }
}
