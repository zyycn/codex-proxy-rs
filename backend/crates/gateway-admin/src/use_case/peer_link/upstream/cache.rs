//! 按账号额度版本缓存互联摘要，完整正文仅在版本变化时分批读取

use super::{
    PeerInfoService, scheduling::SchedulingObservation, service::CachedObservation, snapshot,
};
use crate::model::provider_credentials::ProviderQuota;
use gateway_core::{
    account::{QuotaObservation, UpstreamConnection, UpstreamInfoFailure},
    error::StoreError,
};
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

impl PeerInfoService {
    pub(crate) async fn read_summaries(
        &self,
        connections: &[UpstreamConnection],
    ) -> Result<Vec<(UpstreamConnection, ProviderQuota)>, StoreError> {
        let _refresh = self.cache_refresh.lock().await;
        let ids = connections
            .iter()
            .map(|connection| connection.account_id.clone())
            .collect::<Vec<_>>();
        let versions = self
            .accounts
            .get_quota_versions(&ids)
            .await?
            .into_iter()
            .map(|version| (version.account_id.clone(), version))
            .collect::<BTreeMap<_, _>>();
        let now = SystemTime::now();
        let mut changed = Vec::new();
        let mut replaced = Vec::new();
        {
            let mut observations = self
                .observations
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for connection in connections {
                let observed_at = versions
                    .get(&connection.account_id)
                    .filter(|version| version.credential_revision == connection.credential_revision)
                    .map(|version| version.observed_at);
                if let Some(cached) = observations.get_mut(&connection.account_id)
                    && cached.connection == *connection
                    && cached.observed_at == observed_at
                {
                    cached.checked_at = now;
                } else if observed_at.is_some() {
                    changed.push(connection.clone());
                } else {
                    replaced.push(observations.insert(
                        connection.account_id.clone(),
                        CachedObservation {
                            connection: connection.clone(),
                            observed_at: None,
                            checked_at: now,
                            scheduling: None,
                            summary: None,
                            unsupported: false,
                            unavailable: None,
                        },
                    ));
                }
            }
        }
        drop(replaced);
        // 远端窗口可能很大，逐批投影后释放正文，锁内只交换轻量 Arc
        for batch in changed.chunks(2) {
            let ids = batch
                .iter()
                .map(|connection| connection.account_id.clone())
                .collect::<Vec<_>>();
            let observations = self.accounts.get_quotas(&ids).await?;
            for connection in batch {
                let observation = observations.iter().find(|value| {
                    value.account_id == connection.account_id
                        && value.expected_revision == connection.credential_revision
                });
                self.cache(connection, observation);
            }
        }
        let summaries = {
            let observations = self
                .observations
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            connections
                .iter()
                .map(|connection| {
                    let cached = observations
                        .get(&connection.account_id)
                        .filter(|cached| cached.connection == *connection);
                    (
                        connection.clone(),
                        cached.and_then(|cached| cached.summary.clone()),
                        cached.and_then(|cached| cached.unavailable),
                        cached.is_some_and(|cached| cached.unsupported),
                    )
                })
                .collect::<Vec<_>>()
        };
        Ok(summaries
            .into_iter()
            .map(|(connection, peer, unavailable, unsupported)| {
                let quota = Self::project_peer(
                    &connection,
                    peer.as_deref(),
                    unavailable,
                    self.last_error(&connection)
                        .or(unsupported.then_some(UpstreamInfoFailure::Unsupported)),
                    now,
                );
                (connection, quota)
            })
            .collect())
    }

    pub(super) fn cache(
        &self,
        connection: &UpstreamConnection,
        observation: Option<&QuotaObservation>,
    ) {
        let now = SystemTime::now();
        let peer = observation.and_then(|value| snapshot::decode(&value.quota));
        let scheduling = peer
            .as_ref()
            .and_then(|peer| SchedulingObservation::new(peer, now))
            .filter(|observation| observation.fresh(now))
            .map(Arc::new);
        let summary = peer.map(|mut peer| {
            peer.quota = Default::default();
            peer.models.clear();
            Arc::new(peer)
        });
        let cached = CachedObservation {
            connection: connection.clone(),
            observed_at: observation.map(|value| value.observed_at),
            checked_at: now,
            scheduling,
            summary,
            unsupported: observation
                .is_some_and(|value| snapshot::is_unsupported(&value.quota, connection)),
            unavailable: observation
                .and_then(|value| snapshot::unavailable_reason(&value.quota, connection)),
        };
        let previous = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(connection.account_id.clone(), cached);
        drop(previous);
    }

    pub(super) fn unsupported(&self, connection: &UpstreamConnection) -> bool {
        self.observations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&connection.account_id)
            .is_some_and(|cached| cached.connection == *connection && cached.unsupported)
    }
}
