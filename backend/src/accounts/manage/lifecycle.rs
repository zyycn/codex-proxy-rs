use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
};

use chrono::{DateTime, Duration, Utc};
use futures::{stream, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use tokio::time::sleep;
use uuid::Uuid;

use crate::{
    accounts::{account::AccountStatus, store::AccountStore},
    infra::{
        json::{NumberedPage, Page},
        time::elapsed_millis_i64,
    },
    upstream::openai::token_client::RefreshFailure,
};

use super::{
    types::{
        parse_account_status, stored_to_admin_metadata, AccountHealthCheck, AccountManageError,
        AccountRefreshOutcome, AccountRefreshResult, AccountUpdate, BatchDeleteAccounts,
        ManagedAccount, ManualCreateTokens,
    },
    AccountManageService,
};

const ADMIN_REFRESH_LEASE_TTL_SECONDS: i64 = 5 * 60;

impl AccountManageService {
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<ManagedAccount>, AccountManageError> {
        let page = self
            .store
            .list_metadata(cursor, limit)
            .await
            .map_err(|_| AccountManageError::List)?;
        Ok(Page {
            items: page.items.into_iter().map(ManagedAccount::from).collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        search: Option<String>,
    ) -> Result<NumberedPage<ManagedAccount>, AccountManageError> {
        let page = self
            .store
            .list_metadata_page(page, page_size, search.as_deref())
            .await
            .map_err(|_| AccountManageError::List)?;
        Ok(NumberedPage {
            items: page.items.into_iter().map(ManagedAccount::from).collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        })
    }

    pub async fn get(
        &self,
        account_id: &str,
    ) -> Result<Option<ManagedAccount>, AccountManageError> {
        self.store
            .get_metadata(account_id)
            .await
            .map(|account| account.map(ManagedAccount::from))
            .map_err(|_| AccountManageError::Inspect)
    }
    pub async fn create(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<ManagedAccount, AccountManageError> {
        let provided_refresh_token = crate::accounts::import::normalize_nonempty(refresh_token);
        let tokens = if let Some(access_token) = crate::accounts::import::normalize_nonempty(
            token
                .as_deref()
                .map(crate::accounts::import::normalize_bearer_token),
        ) {
            let claims = crate::accounts::refresh::manual_account_claims(&access_token, Utc::now())
                .map_err(AccountManageError::InvalidToken)?;
            ManualCreateTokens {
                access_token,
                refresh_token_for_new: provided_refresh_token.clone(),
                refresh_token_for_existing: provided_refresh_token,
                claims,
            }
        } else if let Some(refresh_token) = provided_refresh_token {
            let refreshed = self
                .refresh_tokens_from_refresh_token(&refresh_token)
                .await?;
            ManualCreateTokens {
                access_token: refreshed.access_token,
                refresh_token_for_new: refreshed.refresh_token.clone(),
                refresh_token_for_existing: refreshed.refresh_token,
                claims: refreshed.claims,
            }
        } else {
            return Err(AccountManageError::TokenRequired);
        };

        let claims = tokens.claims;
        let existing = if let Some(account_id) = claims.account_id.as_deref() {
            self.store
                .find_by_chatgpt_identity(account_id, claims.user_id.as_deref())
                .await
                .map_err(|_| AccountManageError::Inspect)?
        } else {
            None
        };

        let account_id = if let Some(existing) = existing {
            let updated = self
                .store
                .update_from_claims(
                    &existing.id,
                    crate::accounts::store::AccountClaimsUpdate {
                        email: claims.email.clone(),
                        account_id: claims.account_id.clone(),
                        user_id: claims.user_id.clone(),
                        plan_type: claims.plan_type.clone(),
                        access_token: SecretString::new(tokens.access_token.into()),
                        refresh_token: tokens
                            .refresh_token_for_existing
                            .map(|token| SecretString::new(token.into())),
                        access_token_expires_at: Some(claims.expires_at),
                        next_refresh_at: create_next_refresh_at(
                            self,
                            &existing.id,
                            claims.expires_at,
                            tokens.refresh_token_for_new.is_some()
                                || existing.refresh_token.is_some(),
                        ),
                        status: crate::accounts::account::AccountStatus::Active,
                    },
                )
                .await
                .map_err(|_| AccountManageError::UpdateClaims)?;
            if !updated {
                return Err(AccountManageError::NotFound);
            }
            existing.id
        } else {
            let id = crate::accounts::import::normalized_account_id(None);
            self.store
                .insert(crate::accounts::store::NewAccount {
                    id: id.clone(),
                    email: claims.email.clone(),
                    account_id: claims.account_id.clone(),
                    user_id: claims.user_id.clone(),
                    label: None,
                    plan_type: claims.plan_type.clone(),
                    access_token: SecretString::new(tokens.access_token.into()),
                    refresh_token: tokens
                        .refresh_token_for_new
                        .map(|token| SecretString::new(token.into())),
                    access_token_expires_at: Some(claims.expires_at),
                    status: crate::accounts::account::AccountStatus::Active,
                    added_at: None,
                })
                .await
                .map_err(|_| AccountManageError::Import)?;
            id
        };

        self.sync_account_pool(&account_id).await?;

        self.store
            .get(&account_id)
            .await
            .map_err(|_| AccountManageError::Inspect)?
            .map(stored_to_admin_metadata)
            .ok_or(AccountManageError::NotFound)
    }
    pub async fn update_account(
        &self,
        account_id: &str,
        update: AccountUpdate,
    ) -> Result<Option<ManagedAccount>, AccountManageError> {
        if !update.any() {
            return self.get(account_id).await;
        }
        if update
            .label
            .as_ref()
            .and_then(|label| label.as_ref())
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AccountManageError::LabelTooLong);
        }

        let status = update
            .status
            .as_deref()
            .map(parse_account_status)
            .transpose()?;
        let should_evict = status.is_some();
        let updated = self
            .store
            .update_metadata(
                account_id,
                crate::accounts::store::AccountMetadataUpdate {
                    email: None,
                    account_id: None,
                    user_id: None,
                    label: update.label,
                    plan_type: None,
                    status,
                },
            )
            .await
            .map_err(|_| AccountManageError::UpdateMetadata)?;
        if !updated {
            return Ok(None);
        }

        self.sync_account_pool_best_effort(account_id, "account metadata update")
            .await;
        if should_evict {
            self.evict_account_websocket_pool(account_id).await;
        }
        self.get(account_id).await
    }
    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteAccounts, AccountManageError> {
        if ids.is_empty() {
            return Err(AccountManageError::EmptyIds);
        }
        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.delete(&id).await {
                Ok(true) => {
                    deleted += 1;
                    self.account_pool.remove_account(&id).await;
                    if let Err(error) = self.session_affinity.forget_account(&id).await {
                        tracing::warn!(
                            account_id = id,
                            error = %error,
                            "failed to remove Redis affinities for deleted account"
                        );
                    }
                }
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AccountManageError::Delete),
            }
        }
        Ok(BatchDeleteAccounts { deleted, not_found })
    }
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AccountRefreshResult, AccountManageError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AccountManageError::NotFound),
            Err(_) => return Err(AccountManageError::Inspect),
        };
        let previous_status = account.status;
        let started_at = std::time::Instant::now();

        let skipped = |error: &'static str| AccountRefreshResult {
            id: account.id.clone(),
            email: account.email.clone(),
            previous_status,
            outcome: AccountRefreshOutcome::Skipped,
            error: Some(error.to_string()),
            duration_ms: elapsed_millis_i64(started_at),
        };

        match account.status {
            AccountStatus::Active | AccountStatus::QuotaExhausted => {}
            AccountStatus::Expired => return Ok(skipped("account expired")),
            AccountStatus::Disabled => return Ok(skipped("manually disabled")),
            AccountStatus::Banned => return Ok(skipped("account banned")),
        }

        let Some(refresh_token) = account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string())
        else {
            return Ok(skipped("no refresh token"));
        };

        let lease_now = Utc::now();
        let lease_owner = format!(
            "{}:{}",
            self.refresh_lease_owner_prefix,
            Uuid::new_v4().simple()
        );
        let lease_acquired = self
            .refresh_leases
            .try_acquire(
                account_id,
                &lease_owner,
                lease_now + Duration::seconds(ADMIN_REFRESH_LEASE_TTL_SECONDS),
                lease_now,
            )
            .await
            .map_err(|_| AccountManageError::RefreshLease)?;
        if !lease_acquired {
            return Ok(skipped("refresh already in progress"));
        }

        let refresh_result = self.refresh_tokens_from_refresh_token(&refresh_token).await;
        let release_result = self.refresh_leases.release(account_id, &lease_owner).await;
        if release_result.is_err() {
            return Err(AccountManageError::RefreshLease);
        }

        match refresh_result {
            Ok(refreshed) => {
                let updated = self
                    .store
                    .update_from_claims(
                        account_id,
                        crate::accounts::store::AccountClaimsUpdate {
                            email: refreshed.claims.email,
                            account_id: refreshed.claims.account_id.or(account.account_id),
                            user_id: refreshed.claims.user_id,
                            plan_type: refreshed.claims.plan_type,
                            access_token: SecretString::new(refreshed.access_token.into()),
                            refresh_token: refreshed
                                .refresh_token
                                .map(|token| SecretString::new(token.into())),
                            access_token_expires_at: Some(refreshed.claims.expires_at),
                            next_refresh_at: Some(self.next_refresh_at_for_expires_at(
                                account_id,
                                refreshed.claims.expires_at,
                            )),
                            status: account.status,
                        },
                    )
                    .await
                    .map_err(|_| AccountManageError::UpdateClaims)?;
                if !updated {
                    return Err(AccountManageError::NotFound);
                }
                self.sync_account_pool(account_id).await?;
                Ok(AccountRefreshResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AccountRefreshOutcome::Alive,
                    error: None,
                    duration_ms: elapsed_millis_i64(started_at),
                })
            }
            Err(AccountManageError::RefreshTokenExchange(failure)) => {
                if manual_refresh_failure_is_permanent(&failure) {
                    let status = crate::accounts::account::AccountStatus::Expired;
                    let updated = self
                        .store
                        .set_status(account_id, status)
                        .await
                        .map_err(|_| AccountManageError::UpdateStatus)?;
                    if !updated {
                        return Err(AccountManageError::NotFound);
                    }
                    if crate::accounts::import::refresh_failure_status_clears_next_refresh_at(
                        status,
                    ) {
                        let cleared = self
                            .store
                            .set_next_refresh_at(account_id, None)
                            .await
                            .map_err(|_| AccountManageError::UpdateStatus)?;
                        if !cleared {
                            return Err(AccountManageError::NotFound);
                        }
                    }
                    self.sync_account_pool_best_effort(account_id, "account refresh failure")
                        .await;
                }
                Ok(AccountRefreshResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AccountRefreshOutcome::Dead,
                    error: Some(format!("token refresh exchange failed: {failure}")),
                    duration_ms: elapsed_millis_i64(started_at),
                })
            }
            Err(error) => Err(error),
        }
    }

    pub async fn health_check_accounts(
        &self,
        ids: Option<Vec<String>>,
        stagger_ms: u64,
        concurrency: usize,
    ) -> Result<AccountHealthCheck, AccountManageError> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(|_| AccountManageError::List)?;
        let ids = ids.map(|ids| ids.into_iter().collect::<HashSet<_>>());
        let candidate_ids = accounts
            .into_iter()
            .filter(|account| {
                ids.as_ref()
                    .is_none_or(|ids| ids.contains(account.id.as_str()))
            })
            .map(|account| account.id);
        let results = stream::iter(candidate_ids.enumerate().map(
            |(index, account_id)| async move {
                if index > 0 && stagger_ms > 0 {
                    let base_delay = stagger_ms.saturating_mul(index.min(concurrency) as u64);
                    let delay = stable_jittered_millis(&account_id, base_delay, 0.30);
                    sleep(std::time::Duration::from_millis(delay)).await;
                }
                self.refresh_account(&account_id).await
            },
        ))
        .buffer_unordered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

        Ok(AccountHealthCheck { results })
    }
}

fn create_next_refresh_at(
    service: &AccountManageService,
    account_id: &str,
    expires_at: DateTime<Utc>,
    has_refresh_token: bool,
) -> Option<DateTime<Utc>> {
    has_refresh_token.then(|| service.next_refresh_at_for_expires_at(account_id, expires_at))
}

fn stable_jittered_millis(account_id: &str, base_millis: u64, variance: f64) -> u64 {
    if base_millis == 0 {
        return 0;
    }

    let mut hasher = DefaultHasher::new();
    account_id.hash(&mut hasher);
    "health-check-stagger".hash(&mut hasher);
    let unit = hasher.finish() as f64 / u64::MAX as f64;
    let factor = (1.0 - variance) + unit * variance * 2.0;
    (base_millis as f64 * factor)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

fn manual_refresh_failure_is_permanent(failure: &RefreshFailure) -> bool {
    match failure {
        RefreshFailure::InvalidGrant | RefreshFailure::Banned => true,
        RefreshFailure::RetryableTransport | RefreshFailure::Transport => false,
    }
}
