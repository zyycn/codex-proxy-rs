//! 请求资源的续期任务随 guard 回收；失败不能让仍在执行的请求失去并发约束

use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::lifecycle::{CancellationToken, Deadline, REQUEST_LEASE_TTL};

use crate::StoreResult;

pub(crate) struct LeaseRenewal(tokio::task::JoinHandle<()>);

impl LeaseRenewal {
    pub(crate) fn spawn(
        deadline: Deadline,
        cancellation: Option<CancellationToken>,
        mut renew: impl FnMut(Duration) -> BoxFuture<'static, StoreResult<bool>> + Send + 'static,
    ) -> Self {
        Self(tokio::spawn(async move {
            let cancelled = async {
                match &cancellation {
                    Some(cancellation) => cancellation.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(cancelled);
            let mut expires = tokio::time::Instant::now() + deadline.bounded(REQUEST_LEASE_TTL);
            loop {
                let ttl = deadline.bounded(REQUEST_LEASE_TTL);
                if ttl.is_zero() {
                    break;
                }
                let renewed_until = tokio::time::Instant::now() + ttl;
                let result = tokio::select! {
                    biased;
                    () = &mut cancelled => return,
                    result = tokio::time::timeout_at(expires, renew(ttl)) => result,
                };
                let delay = match result {
                    Ok(Ok(true)) => {
                        expires = renewed_until;
                        REQUEST_LEASE_TTL / 3
                    }
                    Ok(Ok(false)) | Err(_) => break,
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "请求租约续期失败，将在租约有效期内重试");
                        Duration::from_secs(1)
                    }
                };
                tokio::select! {
                    biased;
                    () = &mut cancelled => return,
                    () = tokio::time::sleep_until(expires) => break,
                    () = tokio::time::sleep(delay) => {}
                }
            }
            // 显式执行截止由 Core 归类为 timeout，租约不把它抢先改成 cancelled
            if !deadline.is_elapsed()
                && let Some(cancellation) = &cancellation
            {
                cancellation.cancel();
            }
        }))
    }
}

impl Drop for LeaseRenewal {
    fn drop(&mut self) {
        self.0.abort();
    }
}
