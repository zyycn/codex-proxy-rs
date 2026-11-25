//! 主动额度重置卡查询、按账号串行消费与不确定结果分类

use super::*;

/// 仅索引活跃的消费或排队者，锁资源由请求持有
#[derive(Default)]
pub(super) struct ResetCreditLocks {
    entries:
        std::sync::Mutex<std::collections::HashMap<ProviderAccountId, std::sync::Weak<Mutex<()>>>>,
}

impl ResetCreditLocks {
    fn entry(&self, account_id: &ProviderAccountId) -> ResetCreditEntry<'_> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock = entries
            .get(account_id)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| {
                let lock = Arc::new(Mutex::new(()));
                entries.insert(account_id.clone(), Arc::downgrade(&lock));
                lock
            });
        ResetCreditEntry {
            owner: self,
            account_id: account_id.clone(),
            lock: Some(lock),
        }
    }
}

struct ResetCreditEntry<'a> {
    owner: &'a ResetCreditLocks,
    account_id: ProviderAccountId,
    lock: Option<Arc<Mutex<()>>>,
}

impl ResetCreditEntry<'_> {
    async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock
            .as_ref()
            .expect("账号锁只在 entry 销毁时释放")
            .lock()
            .await
    }
}

impl Drop for ResetCreditEntry<'_> {
    fn drop(&mut self) {
        let mut entries = self
            .owner
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // 引用释放也须在表锁内完成，避免并发 Drop 都看见彼此而留下死 Weak
        // 有持锁或排队者时仍保留同一锁身份；等待 future 取消也走此清理路径
        drop(self.lock.take());
        if entries
            .get(&self.account_id)
            .is_some_and(|lock| lock.strong_count() == 0)
        {
            entries.remove(&self.account_id);
        }
    }
}

/// 主动额度重置卡查询/消费失败
#[derive(Error)]
pub enum CodexResetCreditsError {
    #[error("Codex reset-credit credential data is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before using reset credits")]
    CredentialRefreshRequired { upstream_body: Option<String> },
    #[error("Codex reset-credit account was not found")]
    NotFound,
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex reset-credit upstream returned HTTP {status}")]
    Upstream {
        status: u16,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("Codex reset-credit query transport is unavailable")]
    TransportUnavailable,
    #[error("Codex reset-credit consume result is unknown")]
    ConsumeResultUnknown,
}

impl std::fmt::Debug for CodexResetCreditsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCredentialData => formatter.write_str("InvalidCredentialData"),
            Self::CredentialRefreshRequired { .. } => {
                formatter.write_str("CredentialRefreshRequired { upstream_body: <redacted> }")
            }
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Store { .. } => formatter.write_str("Store { detail: <redacted> }"),
            Self::Upstream {
                status,
                retry_after_seconds,
                ..
            } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .field("body", &"<redacted>")
                .field("retry_after_seconds", retry_after_seconds)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
            Self::ConsumeResultUnknown => formatter.write_str("ConsumeResultUnknown"),
        }
    }
}

enum ResetCreditAttemptError {
    InvalidCredential,
    Upstream(CodexClientError),
}

impl CodexCredentialQuotaService {
    /// 查询当前账号由 Codex Desktop 暴露的主动额度重置卡
    pub async fn list_reset_credits(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexRateLimitResetCredits, CodexResetCreditsError> {
        let account = self.reset_credit_account(account_id).await?;
        let client = self
            .global_profile_client()
            .await
            .map_err(|_| CodexResetCreditsError::TransportUnavailable)?;
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| CodexResetCreditsError::InvalidCredentialData)?;
        let prepared = PreparedCodexRuntimeCredential {
            account,
            credential,
        };
        let request_id = format!("reset_credits_{}", Uuid::now_v7().simple());
        list_reset_credits_once(&client, &prepared, &request_id)
            .await
            .map_err(|error| map_reset_credit_attempt_error(error, false))
    }

    /// 消费一张主动额度重置卡
    /// 相同账号在本进程内串行，且不做传输重试
    pub async fn consume_reset_credit(
        &self,
        account_id: &ProviderAccountId,
        credit_id: Option<&str>,
        redeem_request_id: Uuid,
    ) -> Result<CodexRateLimitResetCreditsConsumeResult, CodexResetCreditsError> {
        let entry = self.reset_consume_locks.entry(account_id);
        let _guard = entry.lock().await;
        let account = self.reset_credit_account(account_id).await?;
        let client = self
            .global_profile_client()
            .await
            .map_err(|_| CodexResetCreditsError::TransportUnavailable)?;
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| CodexResetCreditsError::InvalidCredentialData)?;
        let prepared = PreparedCodexRuntimeCredential {
            account,
            credential,
        };
        let request_id = format!("reset_credit_consume_{}", Uuid::now_v7().simple());
        consume_reset_credit_once(
            &client,
            &prepared,
            &request_id,
            credit_id,
            redeem_request_id,
        )
        .await
        .map_err(|error| map_reset_credit_attempt_error(error, true))
    }

    async fn reset_credit_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, CodexResetCreditsError> {
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|error| CodexResetCreditsError::Store {
                detail: error.to_string(),
            })?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexResetCreditsError::NotFound)?;
        if !access_token_is_current(&account, SystemTime::now()) {
            return Err(CodexResetCreditsError::CredentialRefreshRequired {
                upstream_body: None,
            });
        }
        Ok(account)
    }
}

async fn list_reset_credits_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
    request_id: &str,
) -> Result<CodexRateLimitResetCredits, ResetCreditAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header()
        .map_err(|_| ResetCreditAttemptError::InvalidCredential)?;
    client
        .for_account(&prepared.account)
        .map_err(ResetCreditAttemptError::Upstream)?
        .list_rate_limit_reset_credits(CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            prepared.account.upstream_account_id(),
            request_id,
            None,
        ))
        .await
        .map_err(ResetCreditAttemptError::Upstream)
}

async fn consume_reset_credit_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
    request_id: &str,
    credit_id: Option<&str>,
    redeem_request_id: Uuid,
) -> Result<CodexRateLimitResetCreditsConsumeResult, ResetCreditAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header()
        .map_err(|_| ResetCreditAttemptError::InvalidCredential)?;
    client
        .for_account(&prepared.account)
        .map_err(ResetCreditAttemptError::Upstream)?
        .consume_rate_limit_reset_credit(
            CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                prepared.account.upstream_account_id(),
                request_id,
                None,
            ),
            credit_id,
            redeem_request_id,
        )
        .await
        .map_err(ResetCreditAttemptError::Upstream)
}

fn map_reset_credit_attempt_error(
    error: ResetCreditAttemptError,
    consume: bool,
) -> CodexResetCreditsError {
    match error {
        ResetCreditAttemptError::InvalidCredential => CodexResetCreditsError::InvalidCredentialData,
        ResetCreditAttemptError::Upstream(error) => map_reset_credit_client_error(error, consume),
    }
}

fn map_reset_credit_client_error(error: CodexClientError, consume: bool) -> CodexResetCreditsError {
    match error {
        CodexClientError::Upstream {
            status,
            body,
            diagnostics,
            ..
        } if status == reqwest::StatusCode::UNAUTHORIZED
            && reset_credit_response_was_explicit_rejection(status, &diagnostics) =>
        {
            CodexResetCreditsError::CredentialRefreshRequired {
                upstream_body: Some(body),
            }
        }
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            diagnostics,
            ..
        } => {
            // 2xx 后发生的解码/响应体上限错误使用 synthetic 502 表示，但额度卡
            // 可能已经消费
            // 只有 transport 记录的真实非成功状态与错误状态一致时，
            // 才能把它当作确定的上游拒绝并允许前端清除 pending 幂等键
            if consume && !reset_credit_response_was_explicit_rejection(status, &diagnostics) {
                CodexResetCreditsError::ConsumeResultUnknown
            } else {
                CodexResetCreditsError::Upstream {
                    status: status.as_u16(),
                    body,
                    retry_after_seconds,
                }
            }
        }
        _ if consume => CodexResetCreditsError::ConsumeResultUnknown,
        _ => CodexResetCreditsError::TransportUnavailable,
    }
}

fn reset_credit_response_was_explicit_rejection(
    status: reqwest::StatusCode,
    diagnostics: &crate::transport::CodexUpstreamDiagnostics,
) -> bool {
    !status.is_success() && diagnostics.status_code == Some(status.as_u16())
}
