//! 按实际发送状态记录账号失败和模型冷却

use super::AntigravityProvider;
use gateway_core::{
    account::ProviderAccount,
    error::{ProviderError, ProviderErrorKind as Kind},
    provider_ports::{ProviderCooldownScope, ProviderScopedCooldown},
    routing::UpstreamModelId,
    upstream::UpstreamSendState,
};
use std::time::{Duration, SystemTime};

impl AntigravityProvider {
    pub(super) async fn record_failure(
        &self,
        account: &ProviderAccount,
        model: &UpstreamModelId,
        records_account_feedback: bool,
        error: ProviderError,
    ) -> ProviderError {
        if error.send_state() != UpstreamSendState::Sent
            || !matches!(error.kind(), Kind::Unauthorized | Kind::RateLimited)
        {
            return error;
        }
        // 明确拒绝没有生成结果；换号仍受 Core 的交付屏障、范围与预算约束
        let error = error.with_replay_safe().with_pre_delivery_retry();
        if !records_account_feedback {
            return error;
        }
        let feedback = async {
            match error.kind() {
                Kind::Unauthorized => {
                    // 资源服务拒绝 AT 不能证明 RT 失效，续期仍由已有 OAuth worker 负责
                    self.accounts
                        .expire_access_token(account, SystemTime::now())
                        .await
                        .map_err(|_| ())?;
                }
                Kind::RateLimited => {
                    if let Some(until) = error
                        .retry_after()
                        .and_then(|delay| SystemTime::now().checked_add(delay))
                    {
                        self.cooldowns
                            .put_scoped_if_later(ProviderScopedCooldown::new(
                                account.id().clone(),
                                account.revision(),
                                ProviderCooldownScope::upstream_model(model.clone()),
                                until,
                            ))
                            .await
                            .map_err(|_| ())?;
                    }
                }
                _ => {}
            }
            Ok::<(), ()>(())
        };
        if !matches!(
            tokio::time::timeout(Duration::from_millis(100), feedback).await,
            Ok(Ok(()))
        ) {
            tracing::warn!("Antigravity 失败反馈未能写入共享状态");
        }
        error
    }
}
