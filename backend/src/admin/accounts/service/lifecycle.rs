use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
};

use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use tokio::time::sleep;

use crate::{
    infra::{
        json::{NumberedPage, Page},
        time::elapsed_millis_i64,
    },
    upstream::accounts::store::AccountStore,
};

use super::{
    contracts::{
        parse_account_status, stored_to_admin_metadata, AdminAccountError, AdminAccountHealthCheck,
        AdminAccountMetadata, AdminAccountRefreshOutcome, AdminAccountRefreshResult,
        AdminAccountUpdate, BatchDeleteAccounts, ManualCreateTokens,
    },
    AdminAccountService,
};

impl AdminAccountService {
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AdminAccountMetadata>, AdminAccountError> {
        let page = self
            .store
            .list_metadata(cursor, limit)
            .await
            .map_err(|_| AdminAccountError::List)?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(AdminAccountMetadata::from)
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        search: Option<String>,
    ) -> Result<NumberedPage<AdminAccountMetadata>, AdminAccountError> {
        let page = self
            .store
            .list_metadata_page(page, page_size, search.as_deref())
            .await
            .map_err(|_| AdminAccountError::List)?;
        Ok(NumberedPage {
            items: page
                .items
                .into_iter()
                .map(AdminAccountMetadata::from)
                .collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        })
    }

    pub async fn get(
        &self,
        account_id: &str,
    ) -> Result<Option<AdminAccountMetadata>, AdminAccountError> {
        self.store
            .get_metadata(account_id)
            .await
            .map(|account| account.map(AdminAccountMetadata::from))
            .map_err(|_| AdminAccountError::Inspect)
    }
    pub async fn create(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let provided_refresh_token =
            crate::upstream::accounts::importing::normalize_nonempty(refresh_token);
        let tokens = if let Some(access_token) =
            crate::upstream::accounts::importing::normalize_nonempty(
                token
                    .as_deref()
                    .map(crate::upstream::accounts::importing::normalize_bearer_token),
            ) {
            let claims = crate::upstream::accounts::token_refresh::manual_account_claims(
                &access_token,
                Utc::now(),
            )
            .map_err(AdminAccountError::InvalidToken)?;
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
            return Err(AdminAccountError::TokenRequired);
        };

        let claims = tokens.claims;
        let existing = if let Some(account_id) = claims.account_id.as_deref() {
            self.store
                .find_by_chatgpt_identity(account_id, claims.user_id.as_deref())
                .await
                .map_err(|_| AdminAccountError::Inspect)?
        } else {
            None
        };

        let account_id = if let Some(existing) = existing {
            let updated = self
                .store
                .update_from_claims(
                    &existing.id,
                    crate::upstream::accounts::store::AccountClaimsUpdate {
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
                        status: crate::upstream::accounts::model::AccountStatus::Active,
                    },
                )
                .await
                .map_err(|_| AdminAccountError::UpdateClaims)?;
            if !updated {
                return Err(AdminAccountError::NotFound);
            }
            existing.id
        } else {
            let id = crate::upstream::accounts::importing::normalized_account_id(None);
            self.store
                .insert(crate::upstream::accounts::store::NewAccount {
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
                    status: crate::upstream::accounts::model::AccountStatus::Active,
                    added_at: None,
                })
                .await
                .map_err(|_| AdminAccountError::Import)?;
            id
        };

        self.sync_account_pool(&account_id).await?;

        self.store
            .get(&account_id)
            .await
            .map_err(|_| AdminAccountError::Inspect)?
            .map(stored_to_admin_metadata)
            .ok_or(AdminAccountError::NotFound)
    }
    pub async fn update_account(
        &self,
        account_id: &str,
        update: AdminAccountUpdate,
    ) -> Result<Option<AdminAccountMetadata>, AdminAccountError> {
        if !update.any() {
            return self.get(account_id).await;
        }
        if update
            .label
            .as_ref()
            .and_then(|label| label.as_ref())
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
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
                crate::upstream::accounts::store::AccountMetadataUpdate {
                    email: None,
                    account_id: None,
                    user_id: None,
                    label: update.label,
                    plan_type: None,
                    status,
                },
            )
            .await
            .map_err(|_| AdminAccountError::UpdateMetadata)?;
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
    ) -> Result<BatchDeleteAccounts, AdminAccountError> {
        if ids.is_empty() {
            return Err(AdminAccountError::EmptyIds);
        }
        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.delete(&id).await {
                Ok(true) => {
                    deleted += 1;
                    self.account_pool.remove_account(&id).await;
                }
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AdminAccountError::Delete),
            }
        }
        Ok(BatchDeleteAccounts { deleted, not_found })
    }
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountRefreshResult, AdminAccountError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        };
        let previous_status = account.status;
        let started_at = std::time::Instant::now();

        let skipped = |error: &'static str| AdminAccountRefreshResult {
            id: account.id.clone(),
            email: account.email.clone(),
            previous_status,
            outcome: AdminAccountRefreshOutcome::Skipped,
            error: Some(error.to_string()),
            duration_ms: elapsed_millis_i64(started_at),
        };

        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return Ok(skipped("no refresh token"));
        };
        if account.status == crate::upstream::accounts::model::AccountStatus::Disabled {
            return Ok(skipped("manually disabled"));
        }
        if account.status == crate::upstream::accounts::model::AccountStatus::Refreshing {
            return Ok(skipped("refresh already in progress"));
        }

        match self
            .refresh_tokens_from_refresh_token(refresh_token.expose_secret())
            .await
        {
            Ok(refreshed) => {
                let updated = self
                    .store
                    .update_from_claims(
                        account_id,
                        crate::upstream::accounts::store::AccountClaimsUpdate {
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
                            status: crate::upstream::accounts::model::AccountStatus::Active,
                        },
                    )
                    .await
                    .map_err(|_| AdminAccountError::UpdateClaims)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.sync_account_pool(account_id).await?;
                Ok(AdminAccountRefreshResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AdminAccountRefreshOutcome::Alive,
                    error: None,
                    duration_ms: elapsed_millis_i64(started_at),
                })
            }
            Err(AdminAccountError::RefreshTokenExchange(failure)) => {
                if manual_refresh_failure_is_permanent(&failure) {
                    let status = crate::upstream::accounts::model::AccountStatus::Expired;
                    let updated = self
                        .store
                        .set_status(account_id, status)
                        .await
                        .map_err(|_| AdminAccountError::UpdateStatus)?;
                    if !updated {
                        return Err(AdminAccountError::NotFound);
                    }
                    if crate::upstream::accounts::importing::refresh_failure_status_clears_next_refresh_at(
                        status,
                    ) {
                        let cleared = self
                            .store
                            .set_next_refresh_at(account_id, None)
                            .await
                            .map_err(|_| AdminAccountError::UpdateStatus)?;
                        if !cleared {
                            return Err(AdminAccountError::NotFound);
                        }
                    }
                    self.sync_account_pool_best_effort(account_id, "account refresh failure")
                        .await;
                }
                Ok(AdminAccountRefreshResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AdminAccountRefreshOutcome::Dead,
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
    ) -> Result<AdminAccountHealthCheck, AdminAccountError> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(|_| AdminAccountError::List)?;
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

        Ok(AdminAccountHealthCheck { results })
    }
}

fn create_next_refresh_at(
    service: &AdminAccountService,
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

fn manual_refresh_failure_is_permanent(
    failure: &crate::upstream::accounts::token_refresh::RefreshFailure,
) -> bool {
    match failure {
        crate::upstream::accounts::token_refresh::RefreshFailure::InvalidGrant
        | crate::upstream::accounts::token_refresh::RefreshFailure::Banned => true,
        crate::upstream::accounts::token_refresh::RefreshFailure::RetryableTransport
        | crate::upstream::accounts::token_refresh::RefreshFailure::Transport => false,
    }
}
