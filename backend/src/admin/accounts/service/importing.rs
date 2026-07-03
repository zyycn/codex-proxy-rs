use chrono::Utc;
use secrecy::SecretString;

use super::{
    contracts::{
        import_quota_plan_type, import_status_from_usage_error, import_usage_plan_type,
        import_usage_string, AdminAccountError, ImportSupplementalAccountInfo,
        ImportSupplementalNeeds, ImportedAccountState, ImportedAccounts, ResolvedImportTokens,
    },
    AdminAccountService,
};

impl AdminAccountService {
    pub async fn import(
        &self,
        data: serde_json::Value,
    ) -> Result<ImportedAccounts, AdminAccountError> {
        let parsed = crate::upstream::accounts::importing::parse_account_import_payload(&data)
            .map_err(|_| AdminAccountError::NoImportableAccounts)?;
        let source_format = parsed.source.as_str();
        let entries = parsed.entries;
        if entries.is_empty() {
            return Err(AdminAccountError::NoImportableAccounts);
        }

        let mut imported = 0u32;
        let mut skipped = 0u32;
        for entry in entries {
            match self.import_entry(entry, parsed.source).await? {
                ImportedAccountState::Imported(account_id) => {
                    imported += 1;
                    self.sync_account_pool(&account_id).await?;
                }
                ImportedAccountState::Skipped => skipped += 1,
            }
        }

        Ok(ImportedAccounts {
            imported,
            skipped,
            source_format,
        })
    }
    async fn import_entry(
        &self,
        entry: crate::upstream::accounts::importing::AccountImportEntry,
        source: crate::upstream::accounts::importing::AccountImportSource,
    ) -> Result<ImportedAccountState, AdminAccountError> {
        let Some(resolved_tokens) = self
            .resolve_import_tokens(entry.token, entry.refresh_token)
            .await?
        else {
            return Ok(ImportedAccountState::Skipped);
        };
        let label = crate::upstream::accounts::importing::normalize_label(entry.label);
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
        }

        let access_token_expires_at = entry
            .access_token_expires_at
            .as_deref()
            .map(crate::upstream::accounts::importing::parse_account_import_datetime)
            .transpose()
            .map_err(|_| AdminAccountError::InvalidAccessTokenExpiresAt)?;
        let quota_fetched_at = entry
            .quota_fetched_at
            .as_deref()
            .map(crate::upstream::accounts::importing::parse_account_import_datetime)
            .transpose()
            .map_err(|_| AdminAccountError::InvalidAccessTokenExpiresAt)?;
        let mut quota_json = entry
            .cached_quota
            .as_ref()
            .map(serde_json::Value::to_string);
        let mut quota_fetched_at = quota_fetched_at;
        let quota_verify_required = entry.quota_verify_required.unwrap_or(false);
        let parsed_status = crate::upstream::accounts::importing::parse_account_import_status(
            entry.status.as_deref(),
        )
        .map_err(|error| AdminAccountError::InvalidStatus(error.to_string()))?;
        let mut status = crate::upstream::accounts::importing::normalized_imported_account_status(
            parsed_status,
            source,
            &resolved_tokens.access_token,
        );
        let access_token_expires_at = resolved_tokens
            .claims
            .as_ref()
            .map(|claims| claims.expires_at)
            .or(access_token_expires_at);
        let claims = resolved_tokens.claims.as_ref();
        let mut plan_type = claims
            .and_then(|claims| claims.plan_type.clone())
            .or_else(|| crate::upstream::accounts::importing::normalize_nonempty(entry.plan_type));
        if plan_type.is_none() {
            plan_type = entry.cached_quota.as_ref().and_then(import_quota_plan_type);
        }
        let email = claims.and_then(|claims| claims.email.clone()).or_else(|| {
            crate::upstream::accounts::importing::normalize_nonempty(entry.email.clone())
        });
        let chatgpt_account_id = claims
            .and_then(|claims| claims.account_id.as_deref())
            .or_else(|| {
                crate::upstream::accounts::importing::normalize_nonempty_str(
                    entry.account_id.as_deref(),
                )
            });
        let chatgpt_user_id = claims
            .and_then(|claims| claims.user_id.as_deref())
            .or_else(|| {
                crate::upstream::accounts::importing::normalize_nonempty_str(
                    entry.user_id.as_deref(),
                )
            });
        let supplemental = self
            .import_supplemental_account_info(
                &resolved_tokens.access_token,
                chatgpt_account_id,
                ImportSupplementalNeeds {
                    account_id: chatgpt_account_id.is_none(),
                    user_id: chatgpt_user_id.is_none(),
                    email: email.is_none(),
                    plan_type: plan_type.is_none(),
                    quota: quota_json.is_none(),
                },
            )
            .await;
        let chatgpt_account_id = supplemental
            .account_id
            .or_else(|| chatgpt_account_id.map(ToString::to_string));
        let chatgpt_user_id = chatgpt_user_id
            .map(ToString::to_string)
            .or(supplemental.user_id);
        let email = email.or(supplemental.email);
        let account_id = self
            .import_target_account_id(
                entry.id.as_deref(),
                chatgpt_account_id.as_deref(),
                chatgpt_user_id.as_deref(),
            )
            .await?;
        let refresh_token = resolved_tokens
            .refresh_token
            .map(|token| SecretString::new(token.into()));
        let next_refresh_at = if refresh_token.is_some() {
            access_token_expires_at
                .map(|expires_at| self.next_refresh_at_for_expires_at(&account_id, expires_at))
        } else {
            None
        };
        if plan_type.is_none() {
            plan_type = supplemental.plan_type;
        }
        if quota_json.is_none() {
            quota_json = supplemental.quota_json;
            quota_fetched_at = supplemental.quota_fetched_at.or(quota_fetched_at);
        }
        if let Some(supplemental_status) = supplemental.status {
            status = supplemental_status;
        }
        let account = crate::upstream::accounts::store::NewAccount {
            id: account_id.clone(),
            email,
            account_id: chatgpt_account_id,
            user_id: chatgpt_user_id,
            label,
            plan_type,
            access_token: SecretString::new(resolved_tokens.access_token.into()),
            refresh_token,
            access_token_expires_at,
            status,
            added_at: None,
        };

        match self.store.get(&account_id).await {
            Ok(Some(_)) => {
                let updated = self
                    .store
                    .update_from_import(crate::upstream::accounts::store::ImportedAccountUpdate {
                        account,
                        quota_json,
                        quota_fetched_at,
                        quota_verify_required,
                    })
                    .await
                    .map_err(|_| AdminAccountError::Import)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.store
                    .set_next_refresh_at(&account_id, next_refresh_at)
                    .await
                    .map_err(|_| AdminAccountError::Import)?;
            }
            Ok(None) => {
                self.store
                    .insert(account)
                    .await
                    .map_err(|_| AdminAccountError::Import)?;

                self.store
                    .set_next_refresh_at(&account_id, next_refresh_at)
                    .await
                    .map_err(|_| AdminAccountError::Import)?;

                if quota_json.is_some() || quota_fetched_at.is_some() || quota_verify_required {
                    self.store
                        .apply_imported_quota_state(
                            &account_id,
                            quota_json.as_deref(),
                            quota_fetched_at,
                            quota_verify_required,
                        )
                        .await
                        .map_err(|_| AdminAccountError::Import)?;
                }
            }
            Err(_) => return Err(AdminAccountError::Inspect),
        }

        Ok(ImportedAccountState::Imported(account_id))
    }
    async fn import_supplemental_account_info(
        &self,
        access_token: &str,
        account_id: Option<&str>,
        needs: ImportSupplementalNeeds,
    ) -> ImportSupplementalAccountInfo {
        if !needs.any() {
            return ImportSupplementalAccountInfo::default();
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let context = crate::upstream::transport::CodexRequestContext {
            access_token,
            account_id,
            request_id: &request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id: self.installation_id.as_deref(),
            session_id: None,
        };

        match self.codex.fetch_usage(context).await {
            Ok(raw) => {
                let normalized = crate::upstream::accounts::quota::quota_from_usage(&raw);
                ImportSupplementalAccountInfo {
                    account_id: import_usage_string(&raw, "account_id"),
                    user_id: import_usage_string(&raw, "user_id"),
                    email: import_usage_string(&raw, "email"),
                    plan_type: import_usage_plan_type(&raw),
                    quota_json: serde_json::to_string(&normalized).ok(),
                    quota_fetched_at: Some(Utc::now()),
                    status: None,
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to fetch supplemental account information during import"
                );
                ImportSupplementalAccountInfo {
                    status: import_status_from_usage_error(&error),
                    ..ImportSupplementalAccountInfo::default()
                }
            }
        }
    }
    async fn resolve_import_tokens(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<Option<ResolvedImportTokens>, AdminAccountError> {
        let mut refresh_token =
            crate::upstream::accounts::importing::normalize_nonempty(refresh_token);
        let Some(access_token) = crate::upstream::accounts::importing::normalize_nonempty(
            token
                .as_deref()
                .map(crate::upstream::accounts::importing::normalize_bearer_token),
        ) else {
            let Some(existing_refresh_token) = refresh_token else {
                return Ok(None);
            };
            let refreshed = self
                .token_refresher
                .refresh(&existing_refresh_token)
                .await
                .map_err(AdminAccountError::RefreshTokenExchange)?;
            let access_token = crate::upstream::accounts::importing::normalize_nonempty(Some(
                crate::upstream::accounts::importing::normalize_bearer_token(
                    &refreshed.access_token,
                ),
            ))
            .ok_or(AdminAccountError::TokenRequired)?;
            refresh_token = refreshed.refresh_token;
            let claims = crate::upstream::accounts::token_refresh::manual_account_claims(
                &access_token,
                Utc::now(),
            )
            .map_err(AdminAccountError::InvalidToken)?;
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token,
                claims: Some(claims),
            }));
        };

        if let Ok(claims) = crate::upstream::accounts::token_refresh::manual_account_claims(
            &access_token,
            Utc::now(),
        ) {
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token,
                claims: Some(claims),
            }));
        }

        let Some(existing_refresh_token) = refresh_token else {
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token: None,
                claims: None,
            }));
        };
        let refreshed = self
            .token_refresher
            .refresh(&existing_refresh_token)
            .await
            .map_err(AdminAccountError::RefreshTokenExchange)?;
        let access_token = crate::upstream::accounts::importing::normalize_nonempty(Some(
            crate::upstream::accounts::importing::normalize_bearer_token(&refreshed.access_token),
        ))
        .ok_or(AdminAccountError::TokenRequired)?;
        refresh_token = refreshed.refresh_token;
        let claims = crate::upstream::accounts::token_refresh::manual_account_claims(
            &access_token,
            Utc::now(),
        )
        .map_err(AdminAccountError::InvalidToken)?;
        Ok(Some(ResolvedImportTokens {
            access_token,
            refresh_token,
            claims: Some(claims),
        }))
    }
    async fn import_target_account_id(
        &self,
        id: Option<&str>,
        account_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String, AdminAccountError> {
        let provided_id = crate::upstream::accounts::importing::normalize_nonempty_str(id)
            .map(ToString::to_string);
        if let Some(id) = provided_id.as_deref() {
            match self.store.get(id).await {
                Ok(Some(_)) => return Ok(id.to_string()),
                Ok(None) => {}
                Err(_) => return Err(AdminAccountError::Inspect),
            }
        }

        let chatgpt_account_id =
            crate::upstream::accounts::importing::normalize_nonempty_str(account_id);
        let chatgpt_user_id = crate::upstream::accounts::importing::normalize_nonempty_str(user_id);
        if let Some(chatgpt_account_id) = chatgpt_account_id {
            if let Some(existing) = self
                .store
                .find_by_chatgpt_identity(chatgpt_account_id, chatgpt_user_id)
                .await
                .map_err(|_| AdminAccountError::Inspect)?
            {
                return Ok(existing.id);
            }
        }

        Ok(provided_id
            .unwrap_or_else(|| crate::upstream::accounts::importing::normalized_account_id(None)))
    }
}
