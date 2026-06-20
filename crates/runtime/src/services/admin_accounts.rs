use super::*;

/// 管理端账号服务。
#[derive(Clone)]
pub struct AdminAccountService {
    store: SqliteAccountStore,
    cookies: SqliteCookieStore,
    quota_thresholds: QuotaWarningThresholds,
    codex: Arc<CodexBackendClient>,
    account_pool: Arc<RuntimeAccountPoolService>,
    token_refresher: Arc<dyn TokenRefresher>,
    refresh_margin_seconds: u64,
    installation_id: Option<String>,
}

impl AdminAccountService {
    /// 构造管理端账号服务。
    #[expect(
        clippy::too_many_arguments,
        reason = "constructor wires service dependencies from runtime bootstrap"
    )]
    pub fn new(
        store: SqliteAccountStore,
        cookies: SqliteCookieStore,
        quota_thresholds: QuotaWarningThresholds,
        codex: Arc<CodexBackendClient>,
        account_pool: Arc<RuntimeAccountPoolService>,
        token_refresher: Arc<dyn TokenRefresher>,
        refresh_margin_seconds: u64,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            store,
            cookies,
            quota_thresholds,
            codex,
            account_pool,
            token_refresher,
            refresh_margin_seconds,
            installation_id,
        }
    }

    fn next_refresh_at_for_expires_at(&self, expires_at: DateTime<Utc>) -> DateTime<Utc> {
        let margin_seconds = self.refresh_margin_seconds.min(i64::MAX as u64) as i64;
        expires_at - Duration::seconds(margin_seconds)
    }

    /// 分页列出账号元数据。
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

    /// 返回管理端认证状态摘要。
    pub async fn auth_status(&self) -> Result<AdminAuthStatus, AdminAccountError> {
        let mut cursor = None;
        let mut pool = AdminAuthPoolStatus::default();
        let mut user = None;
        loop {
            let page = self
                .store
                .list_metadata(cursor, 200)
                .await
                .map_err(|_| AdminAccountError::List)?;
            for account in page.items {
                pool.record(account.status);
                if user.is_none() && account.status == AccountStatus::Active {
                    user = Some(AdminAccountMetadata::from(account));
                }
            }
            if page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }

        Ok(AdminAuthStatus {
            authenticated: pool.total > 0,
            user,
            pool,
        })
    }

    /// 清空管理端 OAuth 登录账号。
    pub async fn logout(&self) -> Result<AdminAuthLogout, AdminAccountError> {
        let deleted = self
            .store
            .delete_all()
            .await
            .map_err(|_| AdminAccountError::Delete)?;
        self.account_pool.clear().await;
        Ok(AdminAuthLogout {
            success: true,
            deleted,
        })
    }

    /// 导出账号；包含可重新导入的 token，只应暴露给管理端会话。
    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<AdminStoredAccount>, AdminAccountError> {
        if ids.is_empty() {
            let mut accounts = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminAccountError::Export)?;
                accounts.extend(page.items.into_iter().map(AdminStoredAccount::from));
                if page.next_cursor.is_none() {
                    return Ok(accounts);
                }
                cursor = page.next_cursor;
            }
        }

        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            match self.store.get(&id).await {
                Ok(Some(account)) => accounts.push(AdminStoredAccount::from(account)),
                Ok(None) => {}
                Err(_) => return Err(AdminAccountError::Export),
            }
        }
        Ok(accounts)
    }

    /// 导入 native 账号导出数据。
    pub async fn import(&self, payload: &Value) -> Result<ImportedAccounts, AdminAccountError> {
        let parsed = parse_account_import_payload(payload)?;
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

    /// 手动创建或更新一个经 JWT claims 校验的账号。
    pub async fn create(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let provided_refresh_token = normalize_nonempty(refresh_token);
        let tokens =
            if let Some(access_token) = normalize_nonempty(token.map(normalize_bearer_token)) {
                ManualCreateTokens {
                    access_token,
                    refresh_token_for_new: provided_refresh_token.clone(),
                    refresh_token_for_existing: provided_refresh_token,
                }
            } else if let Some(refresh_token) = provided_refresh_token {
                let token_pair = self
                    .token_refresher
                    .refresh(&refresh_token)
                    .await
                    .map_err(AdminAccountError::RefreshTokenExchange)?;
                let access_token =
                    normalize_nonempty(Some(normalize_bearer_token(token_pair.access_token)))
                        .ok_or(AdminAccountError::TokenRequired)?;
                ManualCreateTokens {
                    access_token,
                    refresh_token_for_new: token_pair
                        .refresh_token
                        .clone()
                        .or_else(|| Some(refresh_token.clone())),
                    refresh_token_for_existing: token_pair.refresh_token,
                }
            } else {
                return Err(AdminAccountError::TokenRequired);
            };

        let claims = manual_account_claims(&tokens.access_token, chrono::Utc::now())
            .map_err(AdminAccountError::InvalidToken)?;
        let existing = self
            .store
            .find_by_chatgpt_identity(&claims.account_id, claims.user_id.as_deref())
            .await
            .map_err(|_| AdminAccountError::Inspect)?;

        let account_id = if let Some(existing) = existing {
            let refresh_token = tokens.refresh_token_for_existing;
            let updated = self
                .store
                .update_from_claims(
                    &existing.id,
                    AccountClaimsUpdate {
                        email: claims.email.clone(),
                        account_id: Some(claims.account_id.clone()),
                        user_id: claims.user_id.clone(),
                        plan_type: claims.plan_type.clone(),
                        access_token: SecretString::new(tokens.access_token.into()),
                        refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
                        access_token_expires_at: Some(claims.expires_at),
                        next_refresh_at: Some(
                            self.next_refresh_at_for_expires_at(claims.expires_at),
                        ),
                        status: AccountStatus::Active,
                    },
                )
                .await
                .map_err(|_| AdminAccountError::UpdateClaims)?;
            if !updated {
                return Err(AdminAccountError::NotFound);
            }
            existing.id
        } else {
            let id = normalized_account_id(None);
            let refresh_token = tokens.refresh_token_for_new;
            self.store
                .insert(NewAccount {
                    id: id.clone(),
                    email: claims.email.clone(),
                    account_id: Some(claims.account_id.clone()),
                    user_id: claims.user_id.clone(),
                    label: None,
                    plan_type: claims.plan_type.clone(),
                    access_token: SecretString::new(tokens.access_token.into()),
                    refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
                    access_token_expires_at: Some(claims.expires_at),
                    status: AccountStatus::Active,
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
            .map(AdminAccountMetadata::from)
            .ok_or(AdminAccountError::NotFound)
    }

    /// 导入 Codex CLI 的 auth.json 内容。
    pub async fn import_codex_cli_auth(
        &self,
        payload: &Value,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let token = first_string(payload, &["access_token", "accessToken", "token"]);
        let refresh_token = first_string(payload, &["refresh_token", "refreshToken"]);
        if token.is_none() && refresh_token.is_none() {
            return Err(AdminAccountError::NoImportableAccounts);
        }
        self.create(token, refresh_token).await
    }

    async fn import_entry(
        &self,
        entry: AccountImportEntry,
        source: AccountImportSource,
    ) -> Result<ImportedAccountState, AdminAccountError> {
        let Some(resolved_tokens) = self
            .resolve_import_tokens(entry.token, entry.refresh_token)
            .await?
        else {
            return Ok(ImportedAccountState::Skipped);
        };
        let label = normalize_label(entry.label);
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
        }

        let access_token_expires_at = entry
            .access_token_expires_at
            .as_deref()
            .map(parse_account_import_datetime)
            .transpose()?;
        let quota_fetched_at = entry
            .quota_fetched_at
            .as_deref()
            .map(parse_account_import_datetime)
            .transpose()?;
        let quota_json = entry.cached_quota.as_ref().map(Value::to_string);
        let quota_verify_required = entry.quota_verify_required.unwrap_or(false);
        let status = normalized_imported_account_status(
            parse_account_import_status(entry.status.as_deref())?,
            source,
            &resolved_tokens.access_token,
        );
        let account_id = self
            .import_target_account_id(
                entry.id.as_deref(),
                resolved_tokens.claims.as_ref(),
                entry.account_id.as_deref(),
                entry.user_id.as_deref(),
            )
            .await?;
        let access_token_expires_at = resolved_tokens
            .claims
            .as_ref()
            .map(|claims| claims.expires_at)
            .or(access_token_expires_at);
        let next_refresh_at = access_token_expires_at
            .map(|expires_at| self.next_refresh_at_for_expires_at(expires_at));
        let claims = resolved_tokens.claims.as_ref();
        let account = NewAccount {
            id: account_id.clone(),
            email: claims
                .and_then(|claims| claims.email.clone())
                .or_else(|| normalize_nonempty(entry.email)),
            account_id: claims
                .map(|claims| claims.account_id.clone())
                .or_else(|| normalize_nonempty(entry.account_id)),
            user_id: claims
                .and_then(|claims| claims.user_id.clone())
                .or_else(|| normalize_nonempty(entry.user_id)),
            label,
            plan_type: claims
                .and_then(|claims| claims.plan_type.clone())
                .or_else(|| normalize_nonempty(entry.plan_type)),
            access_token: SecretString::new(resolved_tokens.access_token.into()),
            refresh_token: resolved_tokens
                .refresh_token
                .map(|token| SecretString::new(token.into())),
            access_token_expires_at,
            status,
        };

        match self.store.get(&account_id).await {
            Ok(Some(_)) => {
                let updated = self
                    .store
                    .update_from_import(ImportedAccountUpdate {
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
                return Ok(ImportedAccountState::Imported(account_id));
            }
            Ok(None) => {}
            Err(_) => return Err(AdminAccountError::Inspect),
        }

        self.store
            .insert(account)
            .await
            .map_err(|_| AdminAccountError::Import)?;

        self.store
            .set_next_refresh_at(&account_id, next_refresh_at)
            .await
            .map_err(|_| AdminAccountError::Import)?;

        if quota_json.is_some()
            || quota_fetched_at.is_some()
            || entry.quota_verify_required.is_some()
        {
            self.store
                .apply_import_quota_state(
                    &account_id,
                    quota_json.as_deref(),
                    quota_fetched_at,
                    quota_verify_required,
                )
                .await
                .map_err(|_| AdminAccountError::Import)?;
        }

        Ok(ImportedAccountState::Imported(account_id))
    }

    async fn resolve_import_tokens(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<Option<ResolvedImportTokens>, AdminAccountError> {
        let mut refresh_token = normalize_nonempty(refresh_token);
        let Some(access_token) = normalize_nonempty(token.map(normalize_bearer_token)) else {
            let Some(existing_refresh_token) = refresh_token else {
                return Ok(None);
            };
            let refreshed = self
                .token_refresher
                .refresh(&existing_refresh_token)
                .await
                .map_err(AdminAccountError::RefreshTokenExchange)?;
            let access_token =
                normalize_nonempty(Some(normalize_bearer_token(refreshed.access_token)))
                    .ok_or(AdminAccountError::TokenRequired)?;
            refresh_token = refreshed.refresh_token.or(Some(existing_refresh_token));
            let claims = manual_account_claims(&access_token, chrono::Utc::now())
                .map_err(AdminAccountError::InvalidToken)?;
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token,
                claims: Some(claims),
            }));
        };

        if let Ok(claims) = manual_account_claims(&access_token, chrono::Utc::now()) {
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
        let access_token = normalize_nonempty(Some(normalize_bearer_token(refreshed.access_token)))
            .ok_or(AdminAccountError::TokenRequired)?;
        refresh_token = refreshed.refresh_token.or(Some(existing_refresh_token));
        let claims = manual_account_claims(&access_token, chrono::Utc::now())
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
        claims: Option<&ManualAccountClaims>,
        account_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String, AdminAccountError> {
        let provided_id = normalize_nonempty_str(id).map(ToString::to_string);
        if let Some(id) = provided_id.as_deref() {
            match self.store.get(id).await {
                Ok(Some(_)) => return Ok(id.to_string()),
                Ok(None) => {}
                Err(_) => return Err(AdminAccountError::Inspect),
            }
        }

        let chatgpt_account_id = claims
            .map(|claims| claims.account_id.as_str())
            .or_else(|| normalize_nonempty_str(account_id));
        let chatgpt_user_id = claims
            .and_then(|claims| claims.user_id.as_deref())
            .or_else(|| normalize_nonempty_str(user_id));
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

        Ok(provided_id.unwrap_or_else(|| normalized_account_id(None)))
    }

    /// 更新账号标签。
    pub async fn update_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, AdminAccountError> {
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
        }
        let updated = self
            .store
            .set_label(account_id, label)
            .await
            .map_err(|_| AdminAccountError::UpdateLabel)?;
        if updated {
            self.sync_account_pool_best_effort(account_id, "account label update")
                .await;
        }
        Ok(updated)
    }

    /// 更新账号状态。
    pub async fn update_status(
        &self,
        account_id: &str,
        status: &str,
    ) -> Result<Option<UpdatedAccountStatus>, AdminAccountError> {
        let status = parse_account_status(status)?;
        match self.store.set_status(account_id, status).await {
            Ok(true) => {
                self.sync_account_pool_best_effort(account_id, "account status update")
                    .await;
                self.evict_account_websocket_pool(account_id).await;
                Ok(Some(UpdatedAccountStatus {
                    id: account_id.to_string(),
                    status,
                }))
            }
            Ok(false) => Ok(None),
            Err(_) => Err(AdminAccountError::UpdateStatus),
        }
    }

    /// 删除账号。
    pub async fn delete(&self, account_id: &str) -> Result<bool, AdminAccountError> {
        let deleted = self
            .store
            .delete(account_id)
            .await
            .map_err(|_| AdminAccountError::Delete)?;
        if deleted {
            self.account_pool.remove_account(account_id).await;
        }
        Ok(deleted)
    }

    /// 批量删除账号。
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

    /// 批量更新账号状态。
    pub async fn batch_update_status(
        &self,
        ids: Vec<String>,
        status: &str,
    ) -> Result<BatchUpdateAccountStatus, AdminAccountError> {
        if ids.is_empty() {
            return Err(AdminAccountError::EmptyIds);
        }
        let status = parse_batch_account_status(status)?;

        let mut updated = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.set_status(&id, status).await {
                Ok(true) => {
                    updated += 1;
                    self.sync_account_pool_best_effort(&id, "account batch status update")
                        .await;
                    self.evict_account_websocket_pool(&id).await;
                }
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AdminAccountError::UpdateStatus),
            }
        }

        Ok(BatchUpdateAccountStatus { updated, not_found })
    }

    /// 使用账号 refresh token 刷新 access token。
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountRefresh, AdminAccountError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        };
        let previous_status = account.status;
        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return Err(AdminAccountError::TokenRequired);
        };

        match self
            .token_refresher
            .refresh(refresh_token.expose_secret())
            .await
        {
            Ok(tokens) => {
                let access_token =
                    normalize_nonempty(Some(normalize_bearer_token(tokens.access_token)))
                        .ok_or(AdminAccountError::TokenRequired)?;
                let claims = manual_account_claims(&access_token, chrono::Utc::now())
                    .map_err(AdminAccountError::InvalidToken)?;
                let updated = self
                    .store
                    .update_from_claims(
                        account_id,
                        AccountClaimsUpdate {
                            email: claims.email,
                            account_id: Some(claims.account_id),
                            user_id: claims.user_id,
                            plan_type: claims.plan_type,
                            access_token: SecretString::new(access_token.into()),
                            refresh_token: tokens
                                .refresh_token
                                .map(|token| SecretString::new(token.into())),
                            access_token_expires_at: Some(claims.expires_at),
                            next_refresh_at: Some(
                                self.next_refresh_at_for_expires_at(claims.expires_at),
                            ),
                            status: AccountStatus::Active,
                        },
                    )
                    .await
                    .map_err(|_| AdminAccountError::UpdateClaims)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.sync_account_pool(account_id).await?;
                Ok(AdminAccountRefresh {
                    id: account_id.to_string(),
                    previous_status,
                    outcome: AdminAccountProbeOutcome::Alive,
                    status: Some(AccountStatus::Active),
                    error: None,
                })
            }
            Err(failure) => {
                let status = refresh_failure_status(failure);
                let updated = self
                    .store
                    .set_status(account_id, status)
                    .await
                    .map_err(|_| AdminAccountError::UpdateStatus)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                if refresh_failure_status_clears_next_refresh_at(status) {
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
                Ok(AdminAccountRefresh {
                    id: account_id.to_string(),
                    previous_status,
                    outcome: AdminAccountProbeOutcome::Dead,
                    status: Some(status),
                    error: Some(failure.to_string()),
                })
            }
        }
    }

    /// 重置账号本地用量计数。
    pub async fn reset_usage(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountResetUsage, AdminAccountError> {
        match self.store.get(account_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        }

        self.store
            .reset_usage(account_id)
            .await
            .map_err(|_| AdminAccountError::ResetUsage)?;
        self.sync_account_pool(account_id).await?;

        Ok(AdminAccountResetUsage {
            id: account_id.to_string(),
            reset: true,
        })
    }

    /// 读取账号 Cookie 请求头。
    pub async fn cookies(&self, account_id: &str) -> Result<Option<String>, AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .cookie_header(account_id, "chatgpt.com")
            .await
            .map_err(|_| AdminAccountError::LoadCookies)
    }

    /// 设置账号 Cookie 请求头。
    pub async fn set_cookies(
        &self,
        account_id: &str,
        cookie_header: &str,
    ) -> Result<Option<String>, AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        match self
            .cookies
            .set_cookie_header(account_id, cookie_header)
            .await
        {
            Ok(0) => Err(AdminAccountError::NoValidCookies),
            Ok(_) => self
                .cookies
                .cookie_header(account_id, "chatgpt.com")
                .await
                .map_err(|_| AdminAccountError::LoadCookies),
            Err(_) => Err(AdminAccountError::StoreCookies),
        }
    }

    /// 删除账号 Cookie。
    pub async fn delete_cookies(&self, account_id: &str) -> Result<(), AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .delete_account_cookies(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::DeleteCookies)
    }

    /// 返回基于缓存配额快照的账号预警。
    pub async fn quota_warnings(&self) -> Result<AdminAccountQuotaWarnings, AdminAccountError> {
        let snapshots = self
            .store
            .list_quota_snapshots()
            .await
            .map_err(|_| AdminAccountError::QuotaWarnings)?;
        Ok(quota_warnings_from_snapshots(
            snapshots,
            &self.quota_thresholds,
        ))
    }

    /// 拉取并持久化单个账号的 Codex usage 配额快照。
    pub async fn account_quota(
        &self,
        account_id: &str,
        request_id: &str,
    ) -> Result<AdminAccountQuota, AdminAccountError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        };
        if account.status != AccountStatus::Active {
            return Err(AdminAccountError::Inactive(account.status));
        }

        let raw = self
            .codex
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
                cookie_header: None,
                installation_id: self.installation_id.as_deref(),
                session_id: None,
            })
            .await
            .map_err(|error| AdminAccountError::FetchQuota(error.to_string()))?;
        let quota = quota_from_usage(&raw);
        let reset_at = quota_snapshot_reset_at(&quota);
        let updated = self
            .store
            .apply_quota_snapshot(
                &account.id,
                &quota.to_string(),
                quota_snapshot_limit_reached(&quota),
                quota_snapshot_limit_reached(&quota)
                    .then_some(reset_at)
                    .flatten(),
            )
            .await
            .map_err(|_| AdminAccountError::StoreQuota)?;
        if !updated {
            return Err(AdminAccountError::NotFound);
        }
        if let Some(reset_at) = reset_at {
            self.store
                .sync_rate_limit_window(
                    &account.id,
                    reset_at,
                    quota_snapshot_limit_window_seconds(&quota),
                )
                .await
                .map_err(|_| AdminAccountError::StoreQuota)?;
        }

        Ok(AdminAccountQuota { quota, raw })
    }

    /// 对账号执行 refresh-token 健康探测。
    pub async fn health_check_accounts(
        &self,
        ids: Option<Vec<String>>,
        concurrency: usize,
        stagger_ms: u64,
        _request_id: &str,
    ) -> Result<Vec<AdminAccountProbeResult>, AdminAccountError> {
        let accounts = self.health_check_candidates(ids).await?;
        let concurrency = concurrency.max(1);
        let results = stream::iter(accounts.into_iter().enumerate())
            .map(|(index, account)| {
                let service = self.clone();
                async move {
                    if stagger_ms > 0 && index > 0 {
                        let multiplier = index.min(concurrency);
                        tokio::time::sleep(std::time::Duration::from_millis(
                            stagger_ms.saturating_mul(multiplier as u64),
                        ))
                        .await;
                    }
                    service.probe_account_refresh(account).await
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        Ok(results)
    }

    async fn health_check_candidates(
        &self,
        ids: Option<Vec<String>>,
    ) -> Result<Vec<StoredAccount>, AdminAccountError> {
        if let Some(ids) = ids {
            let mut accounts = Vec::with_capacity(ids.len());
            for id in ids {
                match self.store.get(&id).await {
                    Ok(Some(account)) => accounts.push(account),
                    Ok(None) => {}
                    Err(_) => return Err(AdminAccountError::HealthCheck),
                }
            }
            return Ok(accounts);
        }

        let mut accounts = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .store
                .list(cursor, 200)
                .await
                .map_err(|_| AdminAccountError::HealthCheck)?;
            accounts.extend(page.items);
            if page.next_cursor.is_none() {
                return Ok(accounts);
            }
            cursor = page.next_cursor;
        }
    }

    async fn probe_account_refresh(&self, account: StoredAccount) -> AdminAccountProbeResult {
        let started_at = Instant::now();
        let previous_status = account.status;
        if account.status == AccountStatus::Disabled {
            return skipped_admin_account_probe_result(account, "manually disabled");
        }
        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return skipped_admin_account_probe_result(account, "no refresh token");
        };

        match self
            .token_refresher
            .refresh(refresh_token.expose_secret())
            .await
        {
            Ok(tokens) => {
                let Some(access_token) =
                    normalize_nonempty(Some(normalize_bearer_token(tokens.access_token)))
                else {
                    return dead_admin_account_probe_result(
                        account,
                        previous_status,
                        "token or refreshToken is required".to_string(),
                        started_at,
                    );
                };
                let claims = match manual_account_claims(&access_token, Utc::now()) {
                    Ok(claims) => claims,
                    Err(error) => {
                        return dead_admin_account_probe_result(
                            account,
                            previous_status,
                            error.to_string(),
                            started_at,
                        );
                    }
                };

                match self
                    .store
                    .update_from_claims(
                        &account.id,
                        AccountClaimsUpdate {
                            email: claims.email,
                            account_id: Some(claims.account_id),
                            user_id: claims.user_id,
                            plan_type: claims.plan_type,
                            access_token: SecretString::new(access_token.into()),
                            refresh_token: tokens
                                .refresh_token
                                .map(|token| SecretString::new(token.into())),
                            access_token_expires_at: Some(claims.expires_at),
                            next_refresh_at: Some(
                                self.next_refresh_at_for_expires_at(claims.expires_at),
                            ),
                            status: AccountStatus::Active,
                        },
                    )
                    .await
                {
                    Ok(true) => {
                        self.sync_account_pool_best_effort(&account.id, "account health refresh")
                            .await;
                        AdminAccountProbeResult {
                            id: account.id,
                            email: account.email,
                            previous_status,
                            outcome: AdminAccountProbeOutcome::Alive,
                            status: Some(AccountStatus::Active),
                            error: None,
                            duration_ms: Some(started_at.elapsed().as_millis()),
                        }
                    }
                    Ok(false) => dead_admin_account_probe_result(
                        account,
                        previous_status,
                        AdminAccountError::NotFound.to_string(),
                        started_at,
                    ),
                    Err(_) => dead_admin_account_probe_result(
                        account,
                        previous_status,
                        AdminAccountError::UpdateClaims.to_string(),
                        started_at,
                    ),
                }
            }
            Err(failure) => {
                let status = health_check_failure_status(failure);
                if let Some(status) = status {
                    match self.store.set_status(&account.id, status).await {
                        Ok(true) => {
                            if refresh_failure_status_clears_next_refresh_at(status) {
                                match self.store.set_next_refresh_at(&account.id, None).await {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        return dead_admin_account_probe_result(
                                            account,
                                            previous_status,
                                            AdminAccountError::NotFound.to_string(),
                                            started_at,
                                        );
                                    }
                                    Err(_) => {
                                        return dead_admin_account_probe_result(
                                            account,
                                            previous_status,
                                            AdminAccountError::UpdateStatus.to_string(),
                                            started_at,
                                        );
                                    }
                                }
                            }
                            self.sync_account_pool_best_effort(
                                &account.id,
                                "account health refresh failure",
                            )
                            .await;
                        }
                        Ok(false) => {
                            return dead_admin_account_probe_result(
                                account,
                                previous_status,
                                AdminAccountError::NotFound.to_string(),
                                started_at,
                            );
                        }
                        Err(_) => {
                            return dead_admin_account_probe_result(
                                account,
                                previous_status,
                                AdminAccountError::UpdateStatus.to_string(),
                                started_at,
                            );
                        }
                    }
                }
                AdminAccountProbeResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AdminAccountProbeOutcome::Dead,
                    status,
                    error: Some(failure.to_string()),
                    duration_ms: Some(started_at.elapsed().as_millis()),
                }
            }
        }
    }

    async fn ensure_cookie_account_exists(
        &self,
        account_id: &str,
    ) -> Result<(), AdminAccountError> {
        match self.cookies.account_exists(account_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AdminAccountError::NotFound),
            Err(_) => Err(AdminAccountError::Inspect),
        }
    }

    async fn sync_account_pool(&self, account_id: &str) -> Result<(), AdminAccountError> {
        self.account_pool
            .sync_account_from_repository(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::SyncAccountPool)
    }

    async fn sync_account_pool_best_effort(&self, account_id: &str, operation: &str) {
        if let Err(error) = self
            .account_pool
            .sync_account_from_repository(account_id)
            .await
        {
            tracing::warn!(
                account_id,
                operation,
                error = %error,
                "failed to sync runtime account pool after admin account update"
            );
        }
    }

    async fn evict_account_websocket_pool(&self, account_id: &str) {
        self.codex.evict_websocket_account(account_id).await;
        match self.store.get(account_id).await {
            Ok(Some(account)) => {
                if let Some(upstream_account_id) = account
                    .account_id
                    .as_deref()
                    .filter(|value| *value != account_id)
                {
                    self.codex
                        .evict_websocket_account(upstream_account_id)
                        .await;
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to inspect account while evicting websocket pool"
                );
            }
        }
    }
}

fn skipped_admin_account_probe_result(
    account: StoredAccount,
    error: &str,
) -> AdminAccountProbeResult {
    AdminAccountProbeResult {
        id: account.id,
        email: account.email,
        previous_status: account.status,
        outcome: AdminAccountProbeOutcome::Skipped,
        status: Some(account.status),
        error: Some(error.to_string()),
        duration_ms: None,
    }
}

fn dead_admin_account_probe_result(
    account: StoredAccount,
    previous_status: AccountStatus,
    error: String,
    started_at: Instant,
) -> AdminAccountProbeResult {
    AdminAccountProbeResult {
        id: account.id,
        email: account.email,
        previous_status,
        outcome: AdminAccountProbeOutcome::Dead,
        status: None,
        error: Some(error),
        duration_ms: Some(started_at.elapsed().as_millis()),
    }
}

fn health_check_failure_status(failure: RefreshFailure) -> Option<AccountStatus> {
    match failure {
        RefreshFailure::InvalidGrant => Some(AccountStatus::Disabled),
        RefreshFailure::QuotaExhausted => Some(AccountStatus::QuotaExhausted),
        RefreshFailure::Banned => Some(AccountStatus::Banned),
        RefreshFailure::Disabled => Some(AccountStatus::Disabled),
        RefreshFailure::RetryableTransport => None,
        RefreshFailure::Transport => None,
    }
}

/// 管理端账号错误。
#[derive(Debug, Error)]
pub enum AdminAccountError {
    /// 列表失败。
    #[error("failed to list accounts")]
    List,
    /// 导出失败。
    #[error("failed to export accounts")]
    Export,
    /// 导入失败。
    #[error("failed to import accounts")]
    Import,
    /// 检查账号失败。
    #[error("failed to inspect account")]
    Inspect,
    /// 更新标签失败。
    #[error("failed to update account label")]
    UpdateLabel,
    /// 更新状态失败。
    #[error("failed to update account status")]
    UpdateStatus,
    /// 删除失败。
    #[error("failed to delete account")]
    Delete,
    /// 重置用量失败。
    #[error("failed to reset account usage")]
    ResetUsage,
    /// 账号不存在。
    #[error("account not found")]
    NotFound,
    /// 读取 Cookie 失败。
    #[error("failed to load account cookies")]
    LoadCookies,
    /// 写入 Cookie 失败。
    #[error("failed to store account cookies")]
    StoreCookies,
    /// 删除 Cookie 失败。
    #[error("failed to delete account cookies")]
    DeleteCookies,
    /// 根据 JWT claims 更新账号失败。
    #[error("failed to update account claims")]
    UpdateClaims,
    /// 读取配额预警失败。
    #[error("failed to load account quota warnings")]
    QuotaWarnings,
    /// 写入配额快照失败。
    #[error("failed to store account quota")]
    StoreQuota,
    /// 拉取配额失败。
    #[error("failed to fetch account quota: {0}")]
    FetchQuota(String),
    /// 健康检查失败。
    #[error("failed to health-check accounts")]
    HealthCheck,
    /// 账号非 active，不能执行需要上游访问的操作。
    #[error("account is {0:?}, cannot query quota")]
    Inactive(AccountStatus),
    /// token 为空。
    #[error("token or refreshToken is required")]
    TokenRequired,
    /// token 非法。
    #[error("{0}")]
    InvalidToken(&'static str),
    /// refresh token 换取 access token 失败。
    #[error("failed to exchange refreshToken: {0}")]
    RefreshTokenExchange(RefreshFailure),
    /// 同步运行时账号池失败。
    #[error("failed to sync runtime account pool")]
    SyncAccountPool,
    /// 没有有效 Cookie。
    #[error("No valid cookies found")]
    NoValidCookies,
    /// 标签过长。
    #[error("account label must be 64 characters or fewer")]
    LabelTooLong,
    /// 状态值无效。
    #[error("unsupported account status: {0}")]
    InvalidStatus(String),
    /// ID 列表为空。
    #[error("account ids are required")]
    EmptyIds,
    /// 没有可导入账号。
    #[error("No importable accounts found")]
    NoImportableAccounts,
    /// access token 过期时间非法。
    #[error("invalid accessTokenExpiresAt")]
    InvalidAccessTokenExpiresAt,
}

/// 管理端账号元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountMetadata {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 账号状态。
    pub status: codex_proxy_core::accounts::model::AccountStatus,
    /// 创建时间。
    pub added_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 管理端账号配额拉取结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAccountQuota {
    /// 归一化后的配额快照。
    pub quota: Value,
    /// Codex usage 原始响应。
    pub raw: Value,
}

/// 管理端可导出的完整账号数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStoredAccount {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// access token 明文。
    pub access_token: String,
    /// refresh token 明文。
    pub refresh_token: Option<String>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 账号状态。
    pub status: codex_proxy_core::accounts::model::AccountStatus,
    /// 创建时间。
    pub added_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 管理端账号配额预警集合。
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAccountQuotaWarnings {
    /// 预警列表。
    pub warnings: Vec<AdminAccountQuotaWarning>,
    /// 产生预警的快照中最新的拉取时间。
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理端账号配额预警。
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAccountQuotaWarning {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 配额窗口。
    pub window: AdminQuotaWarningWindow,
    /// 预警级别。
    pub level: AdminQuotaWarningLevel,
    /// 已使用百分比。
    pub used_percent: f64,
    /// 重置时间戳。
    pub reset_at: Option<i64>,
}

/// 配额预警窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminQuotaWarningWindow {
    /// 主窗口。
    Primary,
    /// 次窗口。
    Secondary,
}

impl AdminQuotaWarningWindow {
    /// 返回 API 字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

/// 配额预警级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminQuotaWarningLevel {
    /// 普通预警。
    Warning,
    /// 临界预警。
    Critical,
}

impl AdminQuotaWarningLevel {
    /// 返回 API 字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// 账号健康探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountProbeResult {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 探测前状态。
    pub previous_status: AccountStatus,
    /// 探测结果。
    pub outcome: AdminAccountProbeOutcome,
    /// 探测后的状态。
    pub status: Option<AccountStatus>,
    /// 错误信息。
    pub error: Option<String>,
    /// 耗时毫秒。
    pub duration_ms: Option<u128>,
}

/// 账号健康探测结果类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAccountProbeOutcome {
    /// 上游 usage 请求成功。
    Alive,
    /// 上游 usage 请求失败。
    Dead,
    /// 未执行上游探测。
    Skipped,
}

impl AdminAccountProbeOutcome {
    /// 返回 API 字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Skipped => "skipped",
        }
    }
}

/// 账号导入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAccounts {
    /// 成功导入数量。
    pub imported: u32,
    /// 跳过数量。
    pub skipped: u32,
    /// 导入适配器格式。
    pub source_format: &'static str,
}

/// 管理端认证状态摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuthStatus {
    /// 是否存在已导入账号。
    pub authenticated: bool,
    /// 当前可展示的 active 账号元数据。
    pub user: Option<AdminAccountMetadata>,
    /// 账号池状态计数。
    pub pool: AdminAuthPoolStatus,
}

/// 管理端认证状态中的账号池计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdminAuthPoolStatus {
    /// 账号总数。
    pub total: u32,
    /// active 账号数。
    pub active: u32,
    /// expired 账号数。
    pub expired: u32,
    /// quota_exhausted 账号数。
    pub quota_exhausted: u32,
    /// refreshing 账号数。
    pub refreshing: u32,
    /// disabled 账号数。
    pub disabled: u32,
    /// banned 账号数。
    pub banned: u32,
}

impl AdminAuthPoolStatus {
    fn record(&mut self, status: AccountStatus) {
        self.total += 1;
        match status {
            AccountStatus::Active => self.active += 1,
            AccountStatus::Expired => self.expired += 1,
            AccountStatus::QuotaExhausted => self.quota_exhausted += 1,
            AccountStatus::Refreshing => self.refreshing += 1,
            AccountStatus::Disabled => self.disabled += 1,
            AccountStatus::Banned => self.banned += 1,
        }
    }
}

/// 管理端登出结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminAuthLogout {
    /// 是否成功。
    pub success: bool,
    /// 删除账号数。
    pub deleted: u64,
}

#[derive(Debug, Clone)]
struct ManualCreateTokens {
    access_token: String,
    refresh_token_for_new: Option<String>,
    refresh_token_for_existing: Option<String>,
}

#[derive(Debug, Clone)]
struct AccountImportEntry {
    id: Option<String>,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    token: Option<String>,
    refresh_token: Option<String>,
    access_token_expires_at: Option<String>,
    status: Option<String>,
    cached_quota: Option<Value>,
    quota_fetched_at: Option<String>,
    quota_verify_required: Option<bool>,
}

#[derive(Debug, Clone)]
struct ResolvedImportTokens {
    access_token: String,
    refresh_token: Option<String>,
    claims: Option<ManualAccountClaims>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportedAccountState {
    Imported(String),
    Skipped,
}

/// 账号状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedAccountStatus {
    /// 账号 ID。
    pub id: String,
    /// 新状态。
    pub status: codex_proxy_core::accounts::model::AccountStatus,
}

/// 批量删除账号结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteAccounts {
    /// 成功删除数量。
    pub deleted: u32,
    /// 未找到的账号 ID。
    pub not_found: Vec<String>,
}

/// 批量状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchUpdateAccountStatus {
    /// 成功更新数量。
    pub updated: u32,
    /// 未找到的账号 ID。
    pub not_found: Vec<String>,
}

/// 管理端手动刷新账号结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountRefresh {
    /// 账号 ID。
    pub id: String,
    /// 刷新前状态。
    pub previous_status: codex_proxy_core::accounts::model::AccountStatus,
    /// 刷新结果。
    pub outcome: AdminAccountProbeOutcome,
    /// 刷新后状态。
    pub status: Option<codex_proxy_core::accounts::model::AccountStatus>,
    /// 错误信息。
    pub error: Option<String>,
}

/// 管理端重置用量结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountResetUsage {
    /// 账号 ID。
    pub id: String,
    /// 是否已处理。
    pub reset: bool,
}

fn parse_account_status(
    status: &str,
) -> Result<codex_proxy_core::accounts::model::AccountStatus, AdminAccountError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(codex_proxy_core::accounts::model::AccountStatus::Active),
        "disabled" => Ok(codex_proxy_core::accounts::model::AccountStatus::Disabled),
        "expired" => Ok(codex_proxy_core::accounts::model::AccountStatus::Expired),
        "quota_exhausted" => Ok(codex_proxy_core::accounts::model::AccountStatus::QuotaExhausted),
        "refreshing" => Ok(codex_proxy_core::accounts::model::AccountStatus::Refreshing),
        "banned" => Ok(codex_proxy_core::accounts::model::AccountStatus::Banned),
        other => Err(AdminAccountError::InvalidStatus(other.to_string())),
    }
}

fn parse_batch_account_status(
    status: &str,
) -> Result<codex_proxy_core::accounts::model::AccountStatus, AdminAccountError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(codex_proxy_core::accounts::model::AccountStatus::Active),
        "disabled" => Ok(codex_proxy_core::accounts::model::AccountStatus::Disabled),
        other => Err(AdminAccountError::InvalidStatus(other.to_string())),
    }
}

fn refresh_failure_status(
    failure: RefreshFailure,
) -> codex_proxy_core::accounts::model::AccountStatus {
    match failure {
        RefreshFailure::InvalidGrant => codex_proxy_core::accounts::model::AccountStatus::Disabled,
        RefreshFailure::QuotaExhausted => {
            codex_proxy_core::accounts::model::AccountStatus::QuotaExhausted
        }
        RefreshFailure::Banned => codex_proxy_core::accounts::model::AccountStatus::Banned,
        RefreshFailure::Disabled => codex_proxy_core::accounts::model::AccountStatus::Disabled,
        RefreshFailure::RetryableTransport => {
            codex_proxy_core::accounts::model::AccountStatus::Active
        }
        RefreshFailure::Transport => codex_proxy_core::accounts::model::AccountStatus::Active,
    }
}

fn refresh_failure_status_clears_next_refresh_at(
    status: codex_proxy_core::accounts::model::AccountStatus,
) -> bool {
    !matches!(
        status,
        codex_proxy_core::accounts::model::AccountStatus::Active
    )
}

const ACCOUNT_IMPORT_ENVELOPE_KEYS: &[&str] =
    &["code", "message", "data", "requestId", "request_id"];
const ACCOUNT_IMPORT_CONTAINER_KEYS: &[&str] = &["sourceFormat", "source_format", "accounts"];
const ACCOUNT_IMPORT_ACCOUNT_KEYS: &[&str] = &[
    "id",
    "email",
    "accountId",
    "account_id",
    "userId",
    "user_id",
    "label",
    "planType",
    "plan_type",
    "token",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "accessTokenExpiresAt",
    "access_token_expires_at",
    "status",
    "addedAt",
    "added_at",
    "updatedAt",
    "updated_at",
];
const SUB2API_ACCOUNT_IMPORT_KEYS: &[&str] = &[
    "id",
    "email",
    "accountId",
    "account_id",
    "userId",
    "user_id",
    "label",
    "planType",
    "plan_type",
    "token",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "status",
    "addedAt",
    "added_at",
    "cachedQuota",
    "cached_quota",
    "quotaFetchedAt",
    "quota_fetched_at",
    "quotaVerifyRequired",
    "quota_verify_required",
    "proxyApiKey",
    "proxy_api_key",
    "usage",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountImportSource {
    Native,
    Sub2api,
}

impl AccountImportSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sub2api => "sub2api",
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedAccountImport {
    source: AccountImportSource,
    entries: Vec<AccountImportEntry>,
}

fn parse_account_import_payload(payload: &Value) -> Result<ParsedAccountImport, AdminAccountError> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("accounts").is_some())
        .map(|data| {
            ensure_account_import_keys(payload, ACCOUNT_IMPORT_ENVELOPE_KEYS)?;
            Ok(data)
        })
        .transpose()?
        .unwrap_or(payload);

    if let Some(accounts) = payload.get("accounts") {
        ensure_account_import_keys(payload, ACCOUNT_IMPORT_CONTAINER_KEYS)?;
        let accounts = accounts
            .as_array()
            .ok_or(AdminAccountError::NoImportableAccounts)?;
        let source = account_import_source(payload, accounts)?;
        return Ok(ParsedAccountImport {
            source,
            entries: parse_account_import_entries(accounts, source)?,
        });
    }
    if let Some(accounts) = payload.as_array() {
        let source = account_import_source(payload, accounts)?;
        return Ok(ParsedAccountImport {
            source,
            entries: parse_account_import_entries(accounts, source)?,
        });
    }

    let source = account_import_source(payload, std::slice::from_ref(payload))?;
    Ok(ParsedAccountImport {
        source,
        entries: account_import_entry_from_value(payload, source)?
            .into_iter()
            .collect(),
    })
}

fn parse_account_import_entries(
    accounts: &[Value],
    source: AccountImportSource,
) -> Result<Vec<AccountImportEntry>, AdminAccountError> {
    let mut entries = Vec::new();
    for account in accounts {
        if let Some(entry) = account_import_entry_from_value(account, source)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn account_import_entry_from_value(
    value: &Value,
    source: AccountImportSource,
) -> Result<Option<AccountImportEntry>, AdminAccountError> {
    let Some(account) = value.as_object() else {
        return Ok(None);
    };
    let allowed_keys = match source {
        AccountImportSource::Native => ACCOUNT_IMPORT_ACCOUNT_KEYS,
        AccountImportSource::Sub2api => SUB2API_ACCOUNT_IMPORT_KEYS,
    };
    if account
        .keys()
        .any(|key| !allowed_keys.contains(&key.as_str()))
    {
        return Err(AdminAccountError::NoImportableAccounts);
    }

    let token = first_string(value, &["token", "accessToken", "access_token"]);
    let refresh_token = first_string(value, &["refreshToken", "refresh_token"]);
    if token.is_none() && refresh_token.is_none() {
        return Ok(None);
    }

    Ok(Some(AccountImportEntry {
        id: first_string(value, &["id"]),
        email: first_string(value, &["email"]),
        account_id: first_string(value, &["accountId", "account_id"]),
        user_id: first_string(value, &["userId", "user_id"]),
        label: first_string(value, &["label"]),
        plan_type: first_string(value, &["planType", "plan_type"]),
        token,
        refresh_token,
        access_token_expires_at: first_string(
            value,
            &["accessTokenExpiresAt", "access_token_expires_at"],
        ),
        status: first_string(value, &["status"]),
        cached_quota: (source == AccountImportSource::Sub2api)
            .then(|| first_value(value, &["cachedQuota", "cached_quota"]))
            .flatten(),
        quota_fetched_at: (source == AccountImportSource::Sub2api)
            .then(|| first_string(value, &["quotaFetchedAt", "quota_fetched_at"]))
            .flatten(),
        quota_verify_required: (source == AccountImportSource::Sub2api)
            .then(|| first_bool(value, &["quotaVerifyRequired", "quota_verify_required"]))
            .flatten(),
    }))
}

fn ensure_account_import_keys(
    value: &Value,
    allowed_keys: &[&str],
) -> Result<(), AdminAccountError> {
    let Some(object) = value.as_object() else {
        return Err(AdminAccountError::NoImportableAccounts);
    };
    if object
        .keys()
        .all(|key| allowed_keys.contains(&key.as_str()))
    {
        Ok(())
    } else {
        Err(AdminAccountError::NoImportableAccounts)
    }
}

fn account_import_source(
    value: &Value,
    accounts: &[Value],
) -> Result<AccountImportSource, AdminAccountError> {
    if let Some(source_format) = first_string(value, &["sourceFormat", "source_format"]) {
        return match source_format.trim().to_ascii_lowercase().as_str() {
            "native" => Ok(AccountImportSource::Native),
            "sub2api" => Ok(AccountImportSource::Sub2api),
            _ => Err(AdminAccountError::NoImportableAccounts),
        };
    }

    if accounts.iter().any(account_import_entry_looks_sub2api) {
        Ok(AccountImportSource::Sub2api)
    } else {
        Ok(AccountImportSource::Native)
    }
}

fn account_import_entry_looks_sub2api(value: &Value) -> bool {
    let Some(account) = value.as_object() else {
        return false;
    };
    [
        "proxyApiKey",
        "proxy_api_key",
        "usage",
        "cachedQuota",
        "cached_quota",
        "quotaFetchedAt",
        "quota_fetched_at",
        "quotaVerifyRequired",
        "quota_verify_required",
    ]
    .iter()
    .any(|key| account.contains_key(*key))
}

fn parse_account_import_status(
    status: Option<&str>,
) -> Result<codex_proxy_core::accounts::model::AccountStatus, AdminAccountError> {
    parse_account_status(status.unwrap_or("active"))
}

fn normalized_imported_account_status(
    status: AccountStatus,
    source: AccountImportSource,
    access_token: &str,
) -> AccountStatus {
    if source == AccountImportSource::Sub2api
        && status == AccountStatus::Active
        && jwt_expiry(access_token, Utc::now()) != JwtExpiry::Valid
    {
        AccountStatus::Expired
    } else {
        status
    }
}

fn parse_account_import_datetime(
    value: &str,
) -> Result<chrono::DateTime<chrono::Utc>, AdminAccountError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|_| AdminAccountError::InvalidAccessTokenExpiresAt)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualAccountClaims {
    account_id: String,
    user_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

fn manual_account_claims(
    token: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ManualAccountClaims, &'static str> {
    let payload = decode_jwt_payload(token).ok_or("Invalid JWT format")?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Token is expired")?;
    if now.timestamp() >= exp {
        return Err("Token is expired");
    }
    let expires_at =
        chrono::DateTime::<chrono::Utc>::from_timestamp(exp, 0).ok_or("Invalid JWT exp claim")?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .ok_or("Token missing chatgpt_account_id claim")?;
    let account_id =
        string_claim(auth, "chatgpt_account_id").ok_or("Token missing chatgpt_account_id claim")?;
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let user_id = string_claim(auth, "chatgpt_user_id")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_user_id")));
    let plan_type = string_claim(auth, "chatgpt_plan_type")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_plan_type")));
    let email = profile.and_then(|profile| string_claim(profile, "email"));

    Ok(ManualAccountClaims {
        account_id,
        user_id,
        email,
        plan_type,
        expires_at,
    })
}

fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

fn string_claim(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn normalize_bearer_token(value: String) -> String {
    value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("bearer "))
        .unwrap_or(value.trim())
        .trim()
        .to_string()
}

fn normalized_account_id(id: Option<String>) -> String {
    normalize_nonempty(id).unwrap_or_else(|| format!("acct_{}", uuid::Uuid::new_v4().simple()))
}

fn normalize_label(value: Option<String>) -> Option<String> {
    normalize_nonempty(value)
}

fn normalize_nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_nonempty_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_bool))
}

fn first_value(value: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter()
        .find_map(|key| value.get(key).filter(|value| !value.is_null()))
        .cloned()
}

fn quota_warnings_from_snapshots(
    snapshots: Vec<AccountQuotaSnapshot>,
    thresholds: &QuotaWarningThresholds,
) -> AdminAccountQuotaWarnings {
    let primary_thresholds = sorted_thresholds(&thresholds.primary);
    let secondary_thresholds = sorted_thresholds(&thresholds.secondary);
    let mut warnings = Vec::new();
    let mut updated_at = None;

    for snapshot in snapshots {
        let Ok(quota) = serde_json::from_str::<Value>(&snapshot.quota_json) else {
            continue;
        };
        let before_len = warnings.len();
        if let Some(warning) = warning_from_quota_window(
            &snapshot.account_id,
            snapshot.email.as_deref(),
            &quota,
            "rate_limit",
            AdminQuotaWarningWindow::Primary,
            &primary_thresholds,
        ) {
            warnings.push(warning);
        }
        if let Some(warning) = warning_from_quota_window(
            &snapshot.account_id,
            snapshot.email.as_deref(),
            &quota,
            "secondary_rate_limit",
            AdminQuotaWarningWindow::Secondary,
            &secondary_thresholds,
        ) {
            warnings.push(warning);
        }
        if warnings.len() > before_len {
            updated_at = max_optional_datetime(updated_at, snapshot.quota_fetched_at);
        }
    }

    AdminAccountQuotaWarnings {
        warnings,
        updated_at,
    }
}

fn warning_from_quota_window(
    account_id: &str,
    email: Option<&str>,
    quota: &Value,
    field: &str,
    window: AdminQuotaWarningWindow,
    thresholds: &[u8],
) -> Option<AdminAccountQuotaWarning> {
    let quota_window = quota.get(field).filter(|value| !value.is_null())?;
    let used_percent = quota_window
        .get("used_percent")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())?;
    let level = warning_level(used_percent, thresholds)?;

    Some(AdminAccountQuotaWarning {
        account_id: account_id.to_string(),
        email: email.map(ToString::to_string),
        window,
        level,
        used_percent,
        reset_at: quota_window.get("reset_at").and_then(Value::as_i64),
    })
}

fn warning_level(used_percent: f64, thresholds: &[u8]) -> Option<AdminQuotaWarningLevel> {
    let matched_index = thresholds
        .iter()
        .rposition(|threshold| quota_reached(used_percent, f64::from(*threshold)))?;
    if matched_index + 1 == thresholds.len() {
        Some(AdminQuotaWarningLevel::Critical)
    } else {
        Some(AdminQuotaWarningLevel::Warning)
    }
}

fn sorted_thresholds(thresholds: &[u8]) -> Vec<u8> {
    let mut thresholds = thresholds.to_vec();
    thresholds.sort_unstable();
    thresholds.dedup();
    thresholds
}

fn max_optional_datetime(
    current: Option<chrono::DateTime<chrono::Utc>>,
    candidate: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

impl From<StoredAccount> for AdminStoredAccount {
    fn from(account: StoredAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            access_token: account.access_token.expose_secret().to_string(),
            refresh_token: account
                .refresh_token
                .map(|token| token.expose_secret().to_string()),
            access_token_expires_at: account.access_token_expires_at,
            status: account.status,
            added_at: account.added_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<StoredAccount> for AdminAccountMetadata {
    fn from(account: StoredAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            access_token_expires_at: account.access_token_expires_at,
            status: account.status,
            added_at: account.added_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<StoredAccountMetadata> for AdminAccountMetadata {
    fn from(account: StoredAccountMetadata) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            access_token_expires_at: account.access_token_expires_at,
            status: account.status,
            added_at: account.added_at,
            updated_at: account.updated_at,
        }
    }
}
