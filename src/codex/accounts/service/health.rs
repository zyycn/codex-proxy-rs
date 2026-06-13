use std::time::Duration as StdDuration;

use chrono::Utc;
use futures::{stream, StreamExt};
use reqwest::StatusCode;
use secrecy::ExposeSecret;
use serde_json::Value;
use tokio::time::sleep;

use crate::{
    codex::accounts::{
        model::AccountStatus,
        repository::{AccountRepository, StoredAccount},
    },
    codex::fingerprint::model::Fingerprint,
    codex::transport::client::{
        build_reqwest_client, CodexBackendClient, CodexClientError, CodexRequestContext,
    },
};

use super::{
    cookies::request_domain, quota::quota_from_usage, AccountProbeOutcome, AccountProbeResult,
    AccountService, HealthCheckError,
};

impl AccountService {
    pub async fn health_check_accounts(
        &self,
        ids: Option<Vec<String>>,
        concurrency: usize,
        stagger_ms: u64,
        request_id: &str,
    ) -> Result<Vec<AccountProbeResult>, HealthCheckError> {
        let repo = self
            .repository
            .as_ref()
            .ok_or(HealthCheckError::RepositoryUnavailable)?;
        let accounts = repo
            .list_all()
            .await
            .map(|accounts| filter_health_check_accounts(accounts, ids.as_deref()))
            .map_err(|_| HealthCheckError::List)?;
        let results = stream::iter(accounts.into_iter().enumerate())
            .map(|(index, account)| {
                let service = self.clone();
                let repo = repo.clone();
                let request_id = request_id.to_string();
                async move {
                    if stagger_ms > 0 && index > 0 {
                        let multiplier = index.min(concurrency);
                        sleep(StdDuration::from_millis(
                            stagger_ms.saturating_mul(multiplier as u64),
                        ))
                        .await;
                    }
                    service
                        .probe_account_with_codex_backend(&repo, account, &request_id)
                        .await
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        Ok(results)
    }

    async fn probe_account_with_codex_backend(
        &self,
        repo: &AccountRepository,
        account: StoredAccount,
        request_id: &str,
    ) -> AccountProbeResult {
        if account.status == AccountStatus::Disabled {
            return skipped_probe_result(&account, "manually disabled");
        }

        let started_at = std::time::Instant::now();
        let previous_status = account.status;
        match fetch_account_usage(self, &account, request_id).await {
            Ok(raw) => {
                let quota = quota_from_usage(&raw);
                let _ = repo
                    .update_quota_json(&account.id, &quota.to_string())
                    .await;
                if account.status != AccountStatus::Active {
                    let _ = repo.set_status(&account.id, AccountStatus::Active).await;
                    self.account_pool
                        .lock()
                        .await
                        .set_status(&account.id, AccountStatus::Active);
                }
                AccountProbeResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AccountProbeOutcome::Alive,
                    status: Some(AccountStatus::Active),
                    error: None,
                    duration_ms: Some(started_at.elapsed().as_millis()),
                }
            }
            Err(error) => {
                let status = apply_codex_account_error(self, repo, &account, &error).await;
                AccountProbeResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AccountProbeOutcome::Dead,
                    status,
                    error: Some(public_codex_error(&error)),
                    duration_ms: Some(started_at.elapsed().as_millis()),
                }
            }
        }
    }
}

pub(super) async fn fetch_account_usage(
    service: &AccountService,
    account: &StoredAccount,
    request_id: &str,
) -> Result<Value, CodexClientError> {
    let cookie_header = account_cookie_header(service, &account.id).await;
    let client = CodexBackendClient::new(
        build_reqwest_client(service.config.tls.force_http11)?,
        service.config.api.base_url.clone(),
        Fingerprint::default_codex_desktop(),
    );
    client
        .fetch_usage(CodexRequestContext {
            access_token: account.access_token.expose_secret(),
            account_id: account.account_id.as_deref(),
            request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: cookie_header.as_deref(),
        })
        .await
}

async fn account_cookie_header(service: &AccountService, account_id: &str) -> Option<String> {
    let domain = request_domain(&service.config.api.base_url)?;
    service
        .cookie_repository
        .as_ref()?
        .cookie_header(account_id, &domain)
        .await
        .ok()
        .flatten()
}

pub(super) async fn apply_codex_account_error(
    service: &AccountService,
    repo: &AccountRepository,
    account: &StoredAccount,
    error: &CodexClientError,
) -> Option<AccountStatus> {
    match classify_codex_account_error(error) {
        Some(CodexAccountErrorAction::SetStatus(status)) => {
            let _ = repo.set_status(&account.id, status).await;
            service
                .account_pool
                .lock()
                .await
                .set_status(&account.id, status);
            Some(status)
        }
        Some(CodexAccountErrorAction::RateLimited {
            retry_after_seconds,
        }) => {
            let cooldown_until = Utc::now() + chrono::Duration::seconds(retry_after_seconds as i64);
            let _ = repo
                .set_quota_cooldown_until(&account.id, cooldown_until)
                .await;
            service
                .account_pool
                .lock()
                .await
                .mark_quota_limited_until(&account.id, cooldown_until);
            None
        }
        Some(CodexAccountErrorAction::CloudflareChallenge { cooldown_seconds }) => {
            let cooldown_until = Utc::now() + chrono::Duration::seconds(cooldown_seconds as i64);
            let _ = repo
                .set_cloudflare_cooldown_until(&account.id, cooldown_until)
                .await;
            service
                .account_pool
                .lock()
                .await
                .set_cloudflare_cooldown_until(&account.id, cooldown_until);
            None
        }
        None => None,
    }
}

pub(super) fn skipped_probe_result(account: &StoredAccount, error: &str) -> AccountProbeResult {
    AccountProbeResult {
        id: account.id.clone(),
        email: account.email.clone(),
        previous_status: account.status,
        outcome: AccountProbeOutcome::Skipped,
        status: Some(account.status),
        error: Some(error.to_string()),
        duration_ms: None,
    }
}

pub(super) fn filter_health_check_accounts(
    accounts: Vec<StoredAccount>,
    ids: Option<&[String]>,
) -> Vec<StoredAccount> {
    let Some(ids) = ids else {
        return accounts;
    };
    accounts
        .into_iter()
        .filter(|account| ids.iter().any(|id| id == &account.id))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexAccountErrorAction {
    SetStatus(AccountStatus),
    RateLimited { retry_after_seconds: u64 },
    CloudflareChallenge { cooldown_seconds: u64 },
}

pub(super) fn classify_codex_account_error(
    error: &CodexClientError,
) -> Option<CodexAccountErrorAction> {
    const DEFAULT_RATE_LIMIT_BACKOFF_SECONDS: u64 = 60;
    const MAX_RATE_LIMIT_BACKOFF_SECONDS: u64 = 3_600;
    const CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS: u64 = 120;

    let CodexClientError::Upstream {
        status,
        body,
        retry_after_seconds,
    } = error
    else {
        return None;
    };
    let lower = body.to_ascii_lowercase();
    if *status == StatusCode::UNAUTHORIZED
        || lower.contains("invalid_grant")
        || lower.contains("invalid_token")
        || lower.contains("access_denied")
        || lower.contains("refresh_token_expired")
        || lower.contains("token_revoked")
    {
        return Some(CodexAccountErrorAction::SetStatus(AccountStatus::Expired));
    }
    if *status == StatusCode::PAYMENT_REQUIRED || lower.contains("quota") {
        return Some(CodexAccountErrorAction::SetStatus(
            AccountStatus::QuotaExhausted,
        ));
    }
    if *status == StatusCode::TOO_MANY_REQUESTS {
        return Some(CodexAccountErrorAction::RateLimited {
            retry_after_seconds: retry_after_seconds
                .unwrap_or(DEFAULT_RATE_LIMIT_BACKOFF_SECONDS)
                .min(MAX_RATE_LIMIT_BACKOFF_SECONDS),
        });
    }
    if *status == StatusCode::FORBIDDEN {
        if is_cloudflare_challenge(body) {
            return Some(CodexAccountErrorAction::CloudflareChallenge {
                cooldown_seconds: CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS,
            });
        }
        return Some(CodexAccountErrorAction::SetStatus(AccountStatus::Banned));
    }
    if lower.contains("account has been deactivated")
        || lower.contains("deactivated")
        || lower.contains("banned")
        || lower.contains("suspended")
    {
        return Some(CodexAccountErrorAction::SetStatus(AccountStatus::Banned));
    }
    None
}

pub(super) fn public_codex_error(error: &CodexClientError) -> String {
    match error {
        CodexClientError::Upstream { status, .. } => {
            format!("upstream returned status {}", status.as_u16())
        }
        CodexClientError::Http(_) => "upstream transport failed".to_string(),
        CodexClientError::InvalidHeaderName(_) | CodexClientError::InvalidHeaderValue(_) => {
            "invalid upstream request headers".to_string()
        }
        CodexClientError::UnsupportedTransport(_)
        | CodexClientError::WebSocket(_)
        | CodexClientError::InvalidSse(_)
        | CodexClientError::ModelsUnavailable => "Codex backend request failed".to_string(),
    }
}

fn is_cloudflare_challenge(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("cf-mitigated")
        || lower.contains("cf-chl-bypass")
        || lower.contains("_cf_chl")
        || lower.contains("cf_chl")
        || lower.contains("attention required")
        || lower.contains("just a moment")
}
