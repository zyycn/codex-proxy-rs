//! Core `ProviderAccountStore` 端口适配与 core 投影映射。

use super::*;

fn loaded_credential_from_record(
    record: ProviderAccountRecord,
) -> Result<LoadedCredential, CoreStoreError> {
    Ok(LoadedCredential {
        account: core_account_from_summary(record.summary)?,
        credential: PlaintextCredential::new(record.provider_credentials_json.fields().clone()),
    })
}

#[async_trait]
impl ProviderAccountStore for PgProviderAccountRepository {
    async fn create_account(&self, account: CoreNewProviderAccount) -> Result<(), CoreStoreError> {
        if account.account.revision().get() != 1 {
            return Err(CoreStoreError::new(CoreStoreErrorKind::InvalidData));
        }
        let credential = JsonObject::try_from_value(
            "provider_credentials_json",
            serde_json::Value::Object(account.credential.into_inner()),
            CREDENTIALS_MAX_BYTES,
        )
        .map_err(core_store_error)?;
        self.insert_provider_account(NewProviderAccount {
            id: account.account.id().as_str().to_owned(),
            provider_kind: account.account.provider().as_str().to_owned(),
            name: account.account.name().to_owned(),
            email: account.account.email().map(str::to_owned),
            upstream_user_id: account.account.upstream_user_id().map(str::to_owned),
            upstream_account_id: account.account.upstream_account_id().map(str::to_owned),
            plan_type: account.account.plan_type().map(str::to_owned),
            authentication_kind: account.account.authentication_kind().to_owned(),
            provider_credentials_json: credential,
            has_refresh_token: account.account.has_refresh_token(),
            access_token_expires_at: account
                .account
                .access_token_expires_at()
                .map(DateTime::<Utc>::from),
            next_refresh_at: account.account.next_refresh_at().map(DateTime::<Utc>::from),
            enabled: account.account.enabled(),
            concurrency_limit: account.account.concurrency_limit(),
            weight: account.account.weight(),
            credential_state: account.account.credential_state(),
            credential_observed_at: Utc::now(),
        })
        .await
        .map_err(core_store_error)
    }

    async fn get_account(
        &self,
        account: &CoreProviderAccountId,
    ) -> Result<Option<CoreProviderAccount>, CoreStoreError> {
        self.load_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?
            .map(|record| core_account_from_summary(record.summary))
            .transpose()
    }

    async fn list_accounts(&self) -> Result<Vec<CoreProviderAccount>, CoreStoreError> {
        self.list_provider_accounts(None, true)
            .await
            .map_err(core_store_error)?
            .into_iter()
            .map(core_account_from_summary)
            .collect()
    }

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<CoreProviderAccount>, CoreStoreError> {
        self.list_provider_accounts(Some(provider.as_str()), false)
            .await
            .map_err(core_store_error)?
            .into_iter()
            .map(core_account_from_summary)
            .collect()
    }

    async fn list_refresh_candidates(
        &self,
        query: CoreProviderRefreshQuery,
    ) -> Result<Vec<LoadedCredential>, CoreStoreError> {
        let excluded_account_ids = query
            .excluded_account_ids()
            .iter()
            .map(|account_id| account_id.as_str())
            .collect::<Vec<_>>();
        let rows = sqlx::query(REFRESH_CANDIDATES_SELECT)
            .bind(query.provider().as_str())
            .bind(DateTime::<Utc>::from(query.refresh_due_before()))
            .bind(DateTime::<Utc>::from(query.force_due_before()))
            .bind(DateTime::<Utc>::from(query.observed_at()))
            .bind(excluded_account_ids)
            .bind(i64::from(query.limit().get()))
            .fetch_all(&self.pool)
            .await
            .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::Unavailable))?;
        rows.into_iter()
            .map(account_record_from_row)
            .map(|record| {
                record
                    .map_err(core_store_error)
                    .and_then(loaded_credential_from_record)
            })
            .collect()
    }

    async fn load_credential(
        &self,
        account: &CoreProviderAccountId,
        expected_revision: CoreCredentialRevision,
    ) -> Result<LoadedCredential, CoreStoreError> {
        let loaded = self.load_current_credential(account).await?;
        if loaded.account.revision().get() != expected_revision.get() {
            return Err(CoreStoreError::new(CoreStoreErrorKind::Conflict));
        }
        Ok(loaded)
    }

    async fn load_current_credential(
        &self,
        account: &CoreProviderAccountId,
    ) -> Result<LoadedCredential, CoreStoreError> {
        let record = self
            .load_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?
            .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
        loaded_credential_from_record(record)
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, CoreStoreError> {
        let CredentialCasUpdateParts {
            account_id,
            expected_revision,
            profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
            account_state,
        } = update.into_parts();
        if profile.account_id != account_id {
            return Err(CoreStoreError::new(CoreStoreErrorKind::InvalidData));
        }
        let account_id = account_id.as_str().to_owned();
        let credentials = JsonObject::try_from_value(
            "provider_credentials_json",
            serde_json::Value::Object(credential.into_inner()),
            CREDENTIALS_MAX_BYTES,
        )
        .map_err(core_store_error)?;
        let credential_state = account_state
            .as_ref()
            .map(|state| state.credential_state.as_str());
        let credential_observed_at = account_state
            .as_ref()
            .map(|state| DateTime::<Utc>::from(state.observed_at));
        let error_reason = account_state
            .as_ref()
            .and_then(|state| state.error_reason.map(AccountErrorReason::as_str));
        let message = account_state
            .as_ref()
            .and_then(|state| state.message.as_deref());
        let next = sqlx::query_scalar::<_, i64>(
            "update provider_accounts
             set name = $3, email = $4, plan_type = $5,
                 provider_credentials_json = $6,
                 credential_revision = credential_revision + 1,
                 has_refresh_token = $7, access_token_expires_at = $8,
                 next_refresh_at = $9,
                 credential_state = case
                   when $10::text is not null and enabled and upstream_user_id is not null
                   then $10 else credential_state end,
                 credential_observed_at = case
                   when $10::text is not null and enabled and upstream_user_id is not null
                   then $11 else credential_observed_at end,
                 last_error_reason = case
                   when $10::text is not null and enabled and upstream_user_id is not null
                   then $12 else last_error_reason end,
                 last_error_message = case
                   when $10::text is not null and enabled and upstream_user_id is not null
                   then $13 else last_error_message end,
                 updated_at = greatest(now(), coalesce($11, now()))
             where id = $1 and credential_revision = $2
             returning credential_revision",
        )
        .bind(&account_id)
        .bind(to_i64(expected_revision.get()).map_err(core_store_error)?)
        .bind(profile.name)
        .bind(profile.email)
        .bind(profile.plan_type)
        .bind(credentials.as_value())
        .bind(has_refresh_token)
        .bind(access_token_expires_at.map(DateTime::<Utc>::from))
        .bind(next_refresh_at.map(DateTime::<Utc>::from))
        .bind(credential_state)
        .bind(credential_observed_at)
        .bind(error_reason)
        .bind(message)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::Unavailable))?;
        match next {
            Some(next) => Ok(CredentialCasOutcome::Updated(
                CoreCredentialRevision::new(to_u64(next).map_err(core_store_error)?)
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
            )),
            None => Ok(CredentialCasOutcome::Conflict),
        }
    }

    async fn get_quotas(
        &self,
        accounts: &[CoreProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, CoreStoreError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let account_ids = accounts
            .iter()
            .map(|account| account.as_str().to_owned())
            .collect::<Vec<_>>();
        let rows = sqlx::query(
            "select id, credential_revision, provider_quota_json, quota_observed_at, \
                    quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at \
             from provider_accounts \
             where id = any($1) and quota_observed_at is not null",
        )
        .bind(account_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::Unavailable))?;
        rows.into_iter()
            .map(|row| {
                let account_id = row
                    .try_get::<String, _>("id")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let revision = row
                    .try_get::<i64, _>("credential_revision")
                    .ok()
                    .and_then(|value| u64::try_from(value).ok())
                    .and_then(|value| CoreCredentialRevision::new(value).ok())
                    .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let quota = row
                    .try_get::<Option<serde_json::Value>, _>("provider_quota_json")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?
                    .and_then(|value| value.as_object().cloned())
                    .map(OpaqueProviderData::new)
                    .unwrap_or_else(|| OpaqueProviderData::new(serde_json::Map::new()));
                let observed_at = row
                    .try_get::<DateTime<Utc>, _>("quota_observed_at")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let access = row
                    .try_get::<String, _>("quota_access_state")
                    .ok()
                    .and_then(|value| QuotaAccessState::parse(&value))
                    .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let evidence = row
                    .try_get::<Option<String>, _>("quota_evidence")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let evidence = match evidence {
                    Some(value) => Some(
                        QuotaEvidence::parse(&value)
                            .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                    ),
                    None => None,
                };
                let reset_at = row
                    .try_get::<Option<DateTime<Utc>>, _>("quota_reset_at")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let access_observed_at = row
                    .try_get::<Option<DateTime<Utc>>, _>("quota_access_observed_at")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let state = QuotaState::from_persisted(
                    access,
                    evidence,
                    access_observed_at.map(Into::into),
                    reset_at.map(Into::into),
                )
                .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                Ok(QuotaObservation {
                    account_id: CoreProviderAccountId::new(account_id)
                        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                    expected_revision: revision,
                    quota,
                    observed_at: observed_at.into(),
                    state,
                })
            })
            .collect()
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, CoreStoreError> {
        let quota = JsonObject::try_from_value(
            "provider_quota_json",
            serde_json::Value::Object(observation.quota.into_inner()),
            QUOTA_MAX_BYTES,
        )
        .map_err(core_store_error)?;
        let updated = self
            .compare_and_swap_provider_quota(
                observation.account_id.as_str(),
                Revision::new(observation.expected_revision.get()).map_err(core_store_error)?,
                quota,
                DateTime::<Utc>::from(observation.observed_at),
                observation.state,
            )
            .await
            .map_err(core_store_error)?;
        Ok(if updated {
            QuotaWriteOutcome::Updated
        } else {
            QuotaWriteOutcome::Conflict
        })
    }

    async fn apply_quota_access(
        &self,
        change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, CoreStoreError> {
        let updated = self
            .apply_provider_quota_access(
                change.account_id.as_str(),
                Revision::new(change.expected_revision.get()).map_err(core_store_error)?,
                change.state,
            )
            .await
            .map_err(core_store_error)?;
        Ok(if updated {
            QuotaWriteOutcome::Updated
        } else {
            QuotaWriteOutcome::Conflict
        })
    }

    async fn touch_quota_observation(
        &self,
        touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, CoreStoreError> {
        let updated = self
            .touch_provider_quota_observation(
                touch.account_id.as_str(),
                Revision::new(touch.expected_revision.get()).map_err(core_store_error)?,
                DateTime::<Utc>::from(touch.observed_at),
            )
            .await
            .map_err(core_store_error)?;
        Ok(if updated {
            QuotaWriteOutcome::Updated
        } else {
            QuotaWriteOutcome::Conflict
        })
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), CoreStoreError> {
        let updated = self
            .apply_provider_account_state(ProviderAccountStateUpdate {
                account_id: change.account_id.as_str().to_owned(),
                expected_revision: Revision::new(change.expected_revision.get())
                    .map_err(core_store_error)?,
                credential_state: change.credential_state,
                credential_observed_at: DateTime::<Utc>::from(change.observed_at),
                error_reason: change.error_reason,
                message: change.message,
            })
            .await
            .map_err(core_store_error)?;
        if updated {
            Ok(())
        } else {
            Err(CoreStoreError::new(CoreStoreErrorKind::Conflict))
        }
    }

    async fn update_account(
        &self,
        update: CoreProviderAccountUpdate,
    ) -> Result<(), CoreStoreError> {
        let updated = self
            .update_provider_account(UpdateProviderAccount {
                id: update.account_id.as_str().to_owned(),
                name: update.name,
                email: update.email,
                plan_type: update.plan_type,
            })
            .await
            .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn set_enabled(
        &self,
        account: &CoreProviderAccountId,
        enabled: bool,
    ) -> Result<(), CoreStoreError> {
        let updated = self
            .set_provider_account_enabled(account.as_str(), enabled)
            .await
            .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn delete_account(&self, account: &CoreProviderAccountId) -> Result<(), CoreStoreError> {
        let deleted = self
            .delete_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?;
        require_core_update(deleted)
    }
}
