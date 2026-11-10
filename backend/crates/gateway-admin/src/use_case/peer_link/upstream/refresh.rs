//! 互联请求的运行期节流、取消恢复与结果提交，协议判断留在 Admin

use super::{PeerInfoService, accounts::store_error, quota, snapshot};
use crate::{
    model::provider_credentials::ProviderQuota,
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::account::{
    OpaqueProviderData, ProviderAccountId, QuotaObservation, QuotaState, QuotaWriteOutcome,
    UpstreamConnection, UpstreamInfoFailure,
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::time::Instant;

pub(super) struct RefreshState {
    connection: UpstreamConnection,
    next_attempt: Instant,
    manual_after: Instant,
    last_error: Option<UpstreamInfoFailure>,
}

impl PeerInfoService {
    fn refresh_state(
        &self,
        connection: &UpstreamConnection,
    ) -> Arc<tokio::sync::Mutex<RefreshState>> {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(connection.account_id.clone())
            .or_insert_with(|| {
                Arc::new(tokio::sync::Mutex::new(RefreshState {
                    connection: connection.clone(),
                    next_attempt: Instant::now(),
                    manual_after: Instant::now(),
                    last_error: None,
                }))
            })
            .clone()
    }

    pub(super) fn refresh_due(&self, connection: &UpstreamConnection) -> bool {
        if self.unsupported(connection) {
            return false;
        }
        let state = self.refresh_state(connection);
        state.try_lock().is_ok_and(|state| {
            state.connection != *connection || Instant::now() >= state.next_attempt
        })
    }

    pub(super) fn last_error(
        &self,
        connection: &UpstreamConnection,
    ) -> Option<UpstreamInfoFailure> {
        let state = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&connection.account_id)?
            .clone();
        state
            .try_lock()
            .ok()
            .filter(|state| state.connection == *connection)
            .and_then(|state| state.last_error)
    }

    pub(crate) async fn refresh(
        &self,
        id: &ProviderAccountId,
        manual: bool,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        // 首查、手动与周期刷新共享容量；排队后再读凭据，避免使用等待期间失效的连接
        let _permit = self
            .refresh_slots
            .acquire()
            .await
            .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::Unavailable))?;
        let loaded = self
            .accounts
            .load_current_credential(id)
            .await
            .map_err(store_error)?;
        let connection = UpstreamConnection::for_account(&loaded.account);
        if !loaded.account.enabled() {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
        }
        let authentication = self
            .connection(&loaded)
            .ok_or_else(|| ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))?;
        let slot = self.refresh_state(&connection);
        let mut state = slot.lock().await;
        if !manual && self.unsupported(&connection)
            || state.connection == connection
                && Instant::now()
                    < if manual {
                        state.manual_after
                    } else {
                        state.next_attempt
                    }
        {
            let error = state.last_error;
            drop(state);
            if let Some(error) = error.and_then(refresh_error) {
                return Err(error);
            }
            return self
                .read_quota(id)
                .await
                .map_err(store_error)
                .map(Option::unwrap_or_default);
        }
        if state.connection != connection {
            state.last_error = None;
        }
        state.connection = connection.clone();
        state.next_attempt = Instant::now() + Duration::from_secs(30);
        state.manual_after = state.next_attempt;
        let started_at = SystemTime::now();
        let (result, retry_after) =
            match self.transport.fetch(&loaded.account, &authentication).await {
                Ok(bytes) => (snapshot::parse(&bytes, SystemTime::now()), None),
                Err(failure) => (Err(failure.kind), failure.retry_after),
            };
        let now = SystemTime::now();
        let failure = result.as_ref().err().copied();
        let delay = match failure {
            None => Duration::from_secs(240 + stable_jitter(id.as_str())),
            Some(UpstreamInfoFailure::Unauthorized | UpstreamInfoFailure::SharingDisabled) => {
                Duration::from_secs(1800)
            }
            Some(_) => Duration::from_secs(30),
        }
        .max(retry_after.unwrap_or_default());
        state.next_attempt = Instant::now() + delay;
        // 管理员修复授权后可主动复核，不继承自动重试的长退避，仍遵守上游限流
        state.manual_after =
            Instant::now() + Duration::from_secs(15).max(retry_after.unwrap_or_default());
        state.last_error = failure;
        let document = match result {
            Ok(document) => Some(document),
            Err(UpstreamInfoFailure::Unsupported) => Some(snapshot::unsupported(&connection)),
            Err(error @ UpstreamInfoFailure::SharingDisabled) => {
                Some(snapshot::unavailable(&connection, error))
            }
            Err(error @ UpstreamInfoFailure::Unauthorized) => {
                let discovered = self
                    .read_summary(id)
                    .await
                    .map_err(store_error)?
                    .and_then(|quota| quota.upstream)
                    .is_some_and(|info| {
                        info.connection_revision == connection.revision && info.discovered
                    });
                // 首次探测失败不能证明对方是互联节点；已确认的连接只留下无共享内容的识别记录
                Some(if discovered {
                    snapshot::unavailable(&connection, error)
                } else {
                    OpaqueProviderData::new(Default::default())
                })
            }
            Err(_) => None,
        };
        if let Some(document) = document {
            let access = snapshot::decode(&document)
                .and_then(|peer| quota::observe(&peer, started_at, now))
                .unwrap_or(QuotaState::unknown());
            let observation = QuotaObservation {
                quota_pause: None,
                expected_connection: Some(connection.clone()),
                account_id: id.clone(),
                expected_revision: connection.credential_revision,
                quota: document,
                plan_type: None,
                observed_at: started_at,
                state: access,
            };
            if self
                .accounts
                .compare_and_swap_quota(observation)
                .await
                .map_err(store_error)?
                == QuotaWriteOutcome::Conflict
            {
                return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
            }
            // 与列表 hydration 串行，避免本地缓存发布旧版本覆盖新版本
            let _refresh = self.cache_refresh.lock().await;
            let observations = self
                .accounts
                .get_quotas(std::slice::from_ref(id))
                .await
                .map_err(store_error)?;
            self.cache(
                &connection,
                observations.iter().find(|observation| {
                    observation.expected_revision == connection.credential_revision
                }),
            );
        }
        drop(state);
        // 不支持协议是正常发现结果，管理端保留分类，后台不重复报警
        if let Some(error) = failure.and_then(refresh_error) {
            return Err(error);
        }
        self.read_quota(id)
            .await
            .map_err(store_error)
            .map(Option::unwrap_or_default)
    }
}

fn stable_jitter(id: &str) -> u64 {
    id.bytes().fold(0_u64, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) % 60
}

fn refresh_error(error: UpstreamInfoFailure) -> Option<ProviderAdminError> {
    let message = match error {
        UpstreamInfoFailure::Unauthorized => "上游未授权读取互联信息",
        UpstreamInfoFailure::Unsupported | UpstreamInfoFailure::SharingDisabled => return None,
        UpstreamInfoFailure::RateLimited => "上游互联查询已限流",
        UpstreamInfoFailure::InvalidResponse => "上游互联响应无效",
        UpstreamInfoFailure::Unavailable => "上游互联服务暂不可用",
    };
    Some(ProviderAdminError::new(ProviderAdminErrorKind::BadGateway).with_public_message(message))
}
