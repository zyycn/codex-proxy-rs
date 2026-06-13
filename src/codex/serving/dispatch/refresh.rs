use axum::http::StatusCode;
use secrecy::SecretString;
use serde_json::json;

use crate::{
    codex::accounts::{
        model::{Account, AccountStatus},
        repository::TokenUpdate,
    },
    codex::gateway::oauth::{RefreshFailure, TokenPair},
    codex::gateway::transport::types::CodexResponsesRequest,
    codex::logs::event::{EventLevel, EventLog},
};

use super::CodexUpstreamDependencies;

pub(super) async fn refresh_account_after_unauthorized(
    deps: &CodexUpstreamDependencies,
    request: &CodexResponsesRequest,
    account: &Account,
    request_id: &str,
) -> Option<Account> {
    if !deps.config.auth.refresh_enabled {
        return None;
    }
    let refresh_token = account.refresh_token.as_deref()?;
    let refresher = deps.token_refresher.as_ref()?;
    match refresher.refresh(refresh_token).await {
        Ok(tokens) => persist_refreshed_account(deps, account, tokens).await,
        Err(failure) => {
            mark_refresh_failure(deps, account, failure, request_id, &request.model).await;
            None
        }
    }
}

async fn persist_refreshed_account(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    tokens: TokenPair,
) -> Option<Account> {
    let repo = deps.account_repository.as_ref()?;
    let access_token = tokens.access_token;
    let refresh_token = tokens.refresh_token;
    repo.update_tokens(
        &account.id,
        TokenUpdate {
            access_token: SecretString::new(access_token.clone().into()),
            refresh_token: refresh_token
                .clone()
                .map(|token| SecretString::new(token.into())),
            access_token_expires_at: None,
        },
    )
    .await
    .ok()?;

    let mut refreshed = account.clone();
    refreshed.access_token = access_token;
    if let Some(refresh_token) = refresh_token {
        refreshed.refresh_token = Some(refresh_token);
    }
    refreshed.status = AccountStatus::Active;
    deps.account_pool.lock().await.insert(refreshed.clone());
    Some(refreshed)
}

async fn mark_refresh_failure(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    failure: RefreshFailure,
    request_id: &str,
    model: &str,
) {
    let status = status_for_refresh_failure(failure);
    if let Some(status) = status {
        if let Some(repo) = deps.account_repository.as_ref() {
            let _ = repo.set_status(&account.id, status).await;
        }
        let mut updated = account.clone();
        updated.status = status;
        deps.account_pool.lock().await.insert(updated);
    }
    log_account_refresh_failure(deps, account, failure, status, request_id, model).await;
}

fn status_for_refresh_failure(failure: RefreshFailure) -> Option<AccountStatus> {
    match failure {
        RefreshFailure::InvalidGrant => Some(AccountStatus::Expired),
        RefreshFailure::QuotaExhausted => Some(AccountStatus::QuotaExhausted),
        RefreshFailure::Banned => Some(AccountStatus::Banned),
        RefreshFailure::Disabled => Some(AccountStatus::Disabled),
        RefreshFailure::Transport => None,
    }
}

async fn log_account_refresh_failure(
    deps: &CodexUpstreamDependencies,
    account: &Account,
    failure: RefreshFailure,
    status: Option<AccountStatus>,
    request_id: &str,
    model: &str,
) {
    let Some(repo) = deps.event_logs.as_ref() else {
        return;
    };
    let mut event = EventLog::new(
        "account.refresh",
        EventLevel::Warn,
        "account refresh failed after upstream 401",
    );
    event.request_id = Some(request_id.to_string());
    event.account_id = Some(account.id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(model.to_string());
    event.status_code = Some(i64::from(StatusCode::UNAUTHORIZED.as_u16()));
    event.metadata = json!({
        "trigger": "upstream_401",
        "failure": refresh_failure_value(failure),
        "accountStatus": status.map(account_status_value),
    });
    if let Err(error) = repo.insert(event).await {
        tracing::warn!(?error, "failed to insert account refresh event log");
    }
}

fn refresh_failure_value(failure: RefreshFailure) -> &'static str {
    match failure {
        RefreshFailure::InvalidGrant => "invalidGrant",
        RefreshFailure::QuotaExhausted => "quotaExhausted",
        RefreshFailure::Banned => "banned",
        RefreshFailure::Disabled => "disabled",
        RefreshFailure::Transport => "transport",
    }
}

fn account_status_value(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quotaExhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}
