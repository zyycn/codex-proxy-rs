//! API Key 上游额度展示的受限查询、快照读取与刷新，不写入原生额度状态

use super::*;
use gateway_admin::{
    model::upstream_quota::AccountUpstreamQuota,
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::account::{UpstreamQuotaRefreshClaim, UpstreamQuotaRefreshResult};
use gateway_protocol::quota::{MAX_QUOTA_RESPONSE_BYTES, QuotaObservationStatus, QuotaResponse};

pub(super) type UpstreamRefreshLock = Mutex<Option<Result<(), ProviderAdminError>>>;

impl CodexCredentialQuotaService {
    pub(crate) async fn upstream_snapshots(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<BTreeMap<ProviderAccountId, AccountUpstreamQuota>, ProviderAdminError> {
        let snapshots = self
            .store
            .upstream_quota_snapshots(ids)
            .await
            .map_err(store_failure)?;
        snapshots
            .into_iter()
            .map(|snapshot| {
                let mut accounts = match snapshot.payload {
                    Some(payload) => {
                        let response: QuotaResponse =
                            serde_json::from_value(Value::Object(payload.into_inner()))
                                .map_err(|_| invalid_response())?;
                        response.validate().map_err(|_| invalid_response())?;
                        response.accounts
                    }
                    None => Vec::new(),
                };
                if snapshot.last_error_code.is_some() {
                    for entry in &mut accounts {
                        if entry.status == QuotaObservationStatus::Observed {
                            entry.status = QuotaObservationStatus::Stale;
                        }
                    }
                }
                Ok((
                    snapshot.account_id,
                    AccountUpstreamQuota {
                        fetched_at: snapshot.fetched_at.map(Into::into),
                        last_attempt_at: snapshot.last_attempt_at.map(Into::into),
                        last_error_code: snapshot.last_error_code,
                        forwarding_blocked: snapshot.forwarding_blocked,
                        accounts,
                    },
                ))
            })
            .collect()
    }

    pub(crate) async fn refresh_upstream_due(&self) -> Result<(), ProviderAdminError> {
        let accounts = self.repository.list_for_provider().await.map_err(|error| {
            ProviderAdminError::new(ProviderAdminErrorKind::Unavailable).with_source(error)
        })?;
        let ids = accounts
            .into_iter()
            .filter(|account| account.enabled() && account.authentication_kind() == "api_key")
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        self.refresh_upstream_ids(&ids).await
    }

    pub(crate) async fn refresh_upstream_ids(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<(), ProviderAdminError> {
        let snapshots = self
            .store
            .upstream_quota_snapshots(ids)
            .await
            .map_err(store_failure)?;
        let now = SystemTime::now();
        let mut pending = futures::stream::iter(snapshots.into_iter().filter(|snapshot| {
            snapshot.last_attempt_at.is_none_or(|at| now.duration_since(at).is_ok_and(|age| age >= Duration::from_secs(60)))
        }).map(|snapshot| async move {
            if let Err(error) = self.refresh_upstream(&snapshot.account_id, false).await {
                tracing::warn!(account_id = %snapshot.account_id, kind = ?error.kind(), "upstream quota refresh failed");
            }
        })).buffer_unordered(4);
        while pending.next().await.is_some() {}
        Ok(())
    }

    pub(crate) async fn refresh_upstream(
        &self,
        id: &ProviderAccountId,
        manual: bool,
    ) -> Result<(), ProviderAdminError> {
        let lock = {
            let mut locks = self.upstream_refresh_locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(id).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(None));
                locks.insert(id.clone(), Arc::downgrade(&lock));
                lock
            }
        };
        let mut result_slot = match lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                let result = lock.lock().await;
                return result.clone().unwrap_or_else(|| {
                    Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict))
                });
            }
        };
        let result = self.refresh_upstream_once(id, manual).await;
        *result_slot = Some(result.clone());
        result
    }

    async fn refresh_upstream_once(
        &self,
        id: &ProviderAccountId,
        manual: bool,
    ) -> Result<(), ProviderAdminError> {
        let loaded = self
            .store
            .load_current_credential(id)
            .await
            .map_err(store_failure)?;
        if loaded.account.provider().as_str() != "openai"
            || loaded.account.authentication_kind() != "api_key"
            || !loaded.account.enabled()
            || !super::super::api_key::quota_setting(&loaded.credential)
                .map_err(|_| invalid_response())?
        {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported));
        }
        let claim = UpstreamQuotaRefreshClaim {
            account_id: id.clone(),
            credential_revision: loaded.account.revision(),
            attempt_id: Uuid::new_v4().to_string(),
            attempted_at: SystemTime::now(),
            minimum_interval: Duration::from_secs(if manual { 10 } else { 60 }),
        };
        if !self
            .store
            .claim_upstream_quota_refresh(&claim)
            .await
            .map_err(store_failure)?
        {
            return if manual {
                Err(ProviderAdminError::new(ProviderAdminErrorKind::RateLimited)
                    .with_public_message("额度刷新过于频繁，请稍后再试"))
            } else {
                Ok(())
            };
        }
        let fetched = fetch(&loaded).await;
        let (payload, error_code) = match fetched {
            Ok(response) => {
                let value = serde_json::to_value(response).map_err(|_| invalid_response())?;
                (
                    Some(OpaqueProviderData::new(
                        value.as_object().cloned().ok_or_else(invalid_response)?,
                    )),
                    None,
                )
            }
            Err(code) => (None, Some(code.to_owned())),
        };
        let failure_code = error_code.clone();
        let written = self
            .store
            .finish_upstream_quota_refresh(UpstreamQuotaRefreshResult {
                claim,
                payload,
                error_code,
            })
            .await
            .map_err(store_failure)?;
        if !written {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict)
                .with_public_message("账号连接已变更，请重新获取额度"));
        }
        match failure_code {
            Some(code) => Err(query_failure(&code)),
            None => Ok(()),
        }
    }
}

async fn fetch(
    loaded: &gateway_core::account::LoadedCredential,
) -> Result<QuotaResponse, &'static str> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let data = crate::credential::CodexCredentialCodec::decode_complete(&loaded.credential)
        .map_err(|_| "invalid_response")?;
    let crate::credential::CodexCredentialData::ApiKey(data) = data else {
        return Err("invalid_response");
    };
    let client = crate::transport::client::build_account_http_client(
        loaded.account.id().as_str(),
        loaded.account.outbound_proxy(),
    )
    .map_err(|_| "network_error")?;
    let url = crate::transport::endpoint_url(&data.base_url, "quota");
    let request = client
        .get(url)
        .bearer_auth(&data.api_key)
        .header("Accept", "application/json")
        .send();
    let mut response = tokio::time::timeout_at(deadline, request)
        .await
        .map_err(|_| "timeout")?
        .map_err(|error| {
            if error.is_timeout() {
                "timeout"
            } else {
                "network_error"
            }
        })?;
    let status = response.status();
    let rejection = match status.as_u16() {
        200..=299 => None,
        401 => Some("unauthorized"),
        403 => Some("forbidden"),
        404 | 405 => Some("unsupported"),
        429 => Some("rate_limited"),
        _ => Some("upstream_error"),
    };
    if let Some(code) = rejection
        && status.as_u16() != 403
    {
        return Err(code);
    }
    // 权限拒绝不能被响应体读取失败覆盖，否则撤销权限后旧快照仍可能继续转发
    if response
        .content_length()
        .is_some_and(|size| size > MAX_QUOTA_RESPONSE_BYTES as u64)
    {
        return Err(rejection.unwrap_or("response_too_large"));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| rejection.unwrap_or("timeout"))?
            .map_err(|_| rejection.unwrap_or("network_error"))?;
        let Some(chunk) = chunk else { break };
        if bytes.len().saturating_add(chunk.len()) > MAX_QUOTA_RESPONSE_BYTES {
            return Err(rejection.unwrap_or("response_too_large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if status.as_u16() == 403 {
        return Err(
            if serde_json::from_slice::<Value>(&bytes)
                .ok()
                .as_ref()
                .and_then(|value| value.pointer("/error/code"))
                .and_then(Value::as_str)
                == Some("quota_sharing_disabled")
            {
                "sharing_disabled"
            } else {
                "forbidden"
            },
        );
    }
    let response: QuotaResponse = serde_json::from_slice(&bytes).map_err(|_| "invalid_response")?;
    response.validate().map_err(|_| "invalid_response")?;
    Ok(response)
}

fn store_failure(error: gateway_core::error::StoreError) -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unavailable).with_source(error)
}

fn invalid_response() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::BadGateway)
        .with_public_message("上游额度格式不兼容")
}

fn query_failure(code: &str) -> ProviderAdminError {
    let message = gateway_admin::model::upstream_quota::quota_error_display(code);
    ProviderAdminError::new(ProviderAdminErrorKind::BadGateway).with_public_message(message)
}
