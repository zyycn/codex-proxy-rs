//! PostgreSQL revision 提交后的可丢失 Redis Pub/Sub 通知。

use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use gateway_core::routing::{
    ConfigRevision,
    snapshot::{SnapshotRevisionStream, SnapshotSubscriptionError, SnapshotSubscriptionPort},
};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::{Revision, StoreError, StoreResult, redis_unavailable, require_nonempty};

use super::{namespace, resource_fingerprint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeChange {
    SnapshotPublished {
        config_revision: Revision,
    },
    ProviderAccountChanged {
        provider_account_fingerprint: String,
        credential_revision: Revision,
    },
}

impl RuntimeChange {
    pub fn provider_account_changed(
        provider_account_id: &str,
        credential_revision: Revision,
    ) -> StoreResult<Self> {
        Ok(Self::ProviderAccountChanged {
            provider_account_fingerprint: resource_fingerprint(
                "runtime change",
                provider_account_id,
            )?,
            credential_revision,
        })
    }
}

pub type RuntimeChangeSubscription =
    Pin<Box<dyn Stream<Item = StoreResult<RuntimeChange>> + Send + 'static>>;

#[async_trait]
pub trait RuntimeChangeRepository: Send + Sync {
    async fn publish_runtime_change(&self, change: &RuntimeChange) -> StoreResult<()>;
    async fn subscribe_runtime_changes(&self) -> StoreResult<RuntimeChangeSubscription>;
}

#[derive(Clone)]
pub struct RedisRuntimeChangeRepository {
    client: redis::Client,
    channel: String,
}

impl RedisRuntimeChangeRepository {
    pub fn new(client: redis::Client, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            client,
            channel: format!("{}:runtime-change:v1", namespace(key_namespace)?),
        })
    }
}

#[async_trait]
impl RuntimeChangeRepository for RedisRuntimeChangeRepository {
    async fn publish_runtime_change(&self, change: &RuntimeChange) -> StoreResult<()> {
        let payload = RuntimeChangeWire::from(change);
        let payload =
            serde_json::to_string(&payload).map_err(|error| invalid(&error.to_string()))?;
        let mut connection = self
            .client
            .get_connection_manager()
            .await
            .map_err(|_| redis_unavailable("connect runtime change publisher"))?;
        connection
            .publish::<_, _, i64>(&self.channel, payload)
            .await
            .map_err(|_| redis_unavailable("publish runtime change"))?;
        Ok(())
    }

    async fn subscribe_runtime_changes(&self) -> StoreResult<RuntimeChangeSubscription> {
        let mut pubsub = self
            .client
            .get_async_pubsub()
            .await
            .map_err(|_| redis_unavailable("connect runtime change subscriber"))?;
        pubsub
            .subscribe(&self.channel)
            .await
            .map_err(|_| redis_unavailable("subscribe runtime changes"))?;
        let stream = pubsub.into_on_message().map(|message| {
            let payload = message
                .get_payload::<String>()
                .map_err(|_| invalid("runtime change payload is not UTF-8"))?;
            let wire: RuntimeChangeWire =
                serde_json::from_str(&payload).map_err(|error| invalid(&error.to_string()))?;
            wire.try_into()
        });
        Ok(Box::pin(stream))
    }
}

impl SnapshotSubscriptionPort for RedisRuntimeChangeRepository {
    fn publish_snapshot_revision(
        &self,
        revision: ConfigRevision,
    ) -> futures::future::BoxFuture<'_, Result<(), SnapshotSubscriptionError>> {
        Box::pin(async move {
            let config_revision = Revision::new(revision.get())
                .map_err(|_| SnapshotSubscriptionError::unavailable())?;
            self.publish_runtime_change(&RuntimeChange::SnapshotPublished { config_revision })
                .await
                .map_err(|_| SnapshotSubscriptionError::unavailable())
        })
    }

    fn subscribe_snapshot_revisions(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<SnapshotRevisionStream, SnapshotSubscriptionError>>
    {
        Box::pin(async move {
            let stream = self
                .subscribe_runtime_changes()
                .await
                .map_err(|_| SnapshotSubscriptionError::unavailable())?
                .filter_map(|change| async move {
                    match change {
                        Ok(RuntimeChange::SnapshotPublished { config_revision }) => Some(
                            ConfigRevision::new(config_revision.get())
                                .map_err(|_| SnapshotSubscriptionError::unavailable()),
                        ),
                        Ok(RuntimeChange::ProviderAccountChanged { .. }) => None,
                        Err(_) => Some(Err(SnapshotSubscriptionError::unavailable())),
                    }
                });
            Ok(Box::pin(stream) as SnapshotRevisionStream)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeChangeWire {
    kind: String,
    revision: u64,
    account: Option<String>,
}

impl From<&RuntimeChange> for RuntimeChangeWire {
    fn from(value: &RuntimeChange) -> Self {
        match value {
            RuntimeChange::SnapshotPublished { config_revision } => Self {
                kind: "snapshot".to_owned(),
                revision: config_revision.get(),
                account: None,
            },
            RuntimeChange::ProviderAccountChanged {
                provider_account_fingerprint,
                credential_revision,
            } => Self {
                kind: "provider_account".to_owned(),
                revision: credential_revision.get(),
                account: Some(provider_account_fingerprint.clone()),
            },
        }
    }
}

impl TryFrom<RuntimeChangeWire> for RuntimeChange {
    type Error = StoreError;

    fn try_from(value: RuntimeChangeWire) -> Result<Self, Self::Error> {
        let revision = Revision::new(value.revision)?;
        match value.kind.as_str() {
            "snapshot" if value.account.is_none() => Ok(Self::SnapshotPublished {
                config_revision: revision,
            }),
            "provider_account" => {
                let account = value
                    .account
                    .ok_or_else(|| invalid("provider account fingerprint is missing"))?;
                require_nonempty("runtime change", "account", &account)?;
                Ok(Self::ProviderAccountChanged {
                    provider_account_fingerprint: account,
                    credential_revision: revision,
                })
            }
            _ => Err(invalid("runtime change kind is invalid")),
        }
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "runtime change",
        message: message.to_owned(),
    }
}
