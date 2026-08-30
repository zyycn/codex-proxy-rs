//! `gateway-admin` 账号端口的 PostgreSQL adapter。

use std::collections::BTreeMap;
use std::sync::Arc;

use gateway_core::provider_ports::ProviderCooldownPort;

use super::*;
use crate::postgres::ObservabilityQueryBudget;

/// Admin 账号用例所需的公共账号、留存观测与 revision 事务能力。
///
/// 三个 PostgreSQL adapter 都保持私有，调用方只能取得 [`AccountStore`] 暴露的领域能力。
#[derive(Clone)]
pub struct PgAdminAccountStore {
    pool: PgPool,
    accounts: PgProviderAccountRepository,
    observability: PgObservabilityRepository,
    control_plane: PgControlPlaneRepository,
    cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
    query_budget: ObservabilityQueryBudget,
}

impl PgAdminAccountStore {
    #[must_use]
    pub fn new(
        pool: PgPool,
        cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
        query_budget: ObservabilityQueryBudget,
    ) -> Self {
        Self {
            pool: pool.clone(),
            accounts: PgProviderAccountRepository::new(pool.clone()),
            observability: PgObservabilityRepository::new(pool.clone(), None, query_budget.clone()),
            control_plane: PgControlPlaneRepository::new(pool),
            cooldowns,
            query_budget,
        }
    }

    async fn usage_observations(
        &self,
        range: ObservabilityRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<ProviderAccountUsageObservation>> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut observations = Vec::with_capacity(account_ids.len());
        for account_ids in account_ids.chunks(ADMIN_USAGE_CHUNK_SIZE) {
            let query = ProviderAccountUsageQuery::for_accounts(range, account_ids.to_vec())
                .and_then(|query| {
                    if range.end.signed_duration_since(range.start) <= TimeDelta::hours(24) {
                        query.with_hourly_request_buckets()
                    } else {
                        Ok(query)
                    }
                })
                .map_err(|error| admin_store_error(ENTITY, error))?;
            observations.extend(
                self.observability
                    .provider_account_usage(query)
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?,
            );
        }
        Ok(observations)
    }

    async fn usage_by_windows(
        &self,
        windows: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        let account_ids = windows
            .iter()
            .map(|window| window.account_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        validate_admin_account_ids(&account_ids)
            .map_err(|error| admin_store_error(ENTITY, error))?;
        for window in windows {
            require_nonempty(ENTITY, "quota window key", &window.key)
                .map_err(|error| admin_store_error(ENTITY, error))?;
            ObservabilityRange::new(window.range.start, window.range.end)
                .map_err(|error| admin_store_error(ENTITY, error))?;
        }
        let keys = windows
            .iter()
            .map(|window| window.key.clone())
            .collect::<Vec<_>>();
        let starts = windows
            .iter()
            .map(|window| window.range.start)
            .collect::<Vec<_>>();
        let ends = windows
            .iter()
            .map(|window| window.range.end)
            .collect::<Vec<_>>();
        let account_ids = windows
            .iter()
            .map(|window| window.account_id.clone())
            .collect::<Vec<_>>();
        let rows = self
            .query_budget
            .run("load account usage windows", async {
                sqlx::query(ACCOUNT_USAGE_BY_WINDOWS_SQL)
                    .bind(account_ids)
                    .bind(keys)
                    .bind(starts)
                    .bind(ends)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|_| postgres_unavailable("load provider account quota window usage"))
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let mut usage_rows = Vec::with_capacity(windows.len());
        let mut costs_by_window = BTreeMap::<(String, String), Vec<AccountCost>>::new();
        let mut model_costs = BTreeMap::<(String, String, String), Vec<AccountCost>>::new();
        let mut models_by_window = BTreeMap::<(String, String), Vec<AccountModelUsage>>::new();
        for row in &rows {
            let model_grouping = window_usage_value::<i32>(row, "model_grouping")?;
            let currency_grouping = window_usage_value::<i32>(row, "currency_grouping")?;
            match (model_grouping, currency_grouping) {
                (1, 1) => usage_rows.push(row),
                (1, 0) if window_usage_value::<Option<String>>(row, "cost_currency")?.is_some() => {
                    let (key, cost) = admin_account_usage_window_cost(row)?;
                    costs_by_window.entry(key).or_default().push(cost);
                }
                (0, 1) if window_usage_value::<Option<String>>(row, "model")?.is_some() => {
                    let ((account_id, window_key, _), usage) =
                        admin_account_usage_window_model(row)?;
                    models_by_window
                        .entry((account_id, window_key))
                        .or_default()
                        .push(usage);
                }
                (0, 0)
                    if window_usage_value::<Option<String>>(row, "model")?.is_some()
                        && window_usage_value::<Option<String>>(row, "cost_currency")?
                            .is_some() =>
                {
                    let (key, cost) = admin_account_usage_window_model_cost(row)?;
                    model_costs.entry(key).or_default().push(cost);
                }
                (0 | 1, 0 | 1) => {}
                _ => {
                    return Err(AdminStoreError::new(
                        AdminStoreErrorKind::Unavailable,
                        ENTITY,
                        "account usage window query returned an invalid grouping marker",
                    ));
                }
            }
        }
        let mut results = usage_rows
            .into_iter()
            .map(admin_account_usage_window)
            .collect::<AdminStoreResult<Vec<_>>>()?;
        for result in &mut results {
            result.usage.costs = costs_by_window
                .remove(&(result.account_id.clone(), result.key.clone()))
                .unwrap_or_default();
            result.usage.models = models_by_window
                .remove(&(result.account_id.clone(), result.key.clone()))
                .unwrap_or_default();
            for model in &mut result.usage.models {
                model.costs = model_costs
                    .remove(&(
                        result.account_id.clone(),
                        result.key.clone(),
                        model.model.clone(),
                    ))
                    .unwrap_or_default();
            }
            result.usage.models.sort_by(|left, right| {
                right
                    .request_count
                    .cmp(&left.request_count)
                    .then_with(|| left.model.cmp(&right.model))
            });
        }
        Ok(results)
    }

    async fn required_scope(
        &self,
        account_id: &str,
    ) -> AdminStoreResult<ProviderAccountAdminScope> {
        let record = self
            .accounts
            .load_provider_account(account_id)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| {
                admin_store_error(
                    ENTITY,
                    StoreError::NotFound {
                        entity: ENTITY,
                        id: account_id.to_owned(),
                    },
                )
            })?;
        Ok(ProviderAccountAdminScope {
            provider_kind: record.summary.provider_kind,
        })
    }

    async fn commit_prepared_import(
        &self,
        prepared: PreparedCredentialImport,
        context: &MutationContext,
        action: &str,
    ) -> AdminStoreResult<CredentialImportResult> {
        let provider_kind = prepared.provider_kind.as_str().to_owned();
        let accounts = prepared
            .credentials
            .into_iter()
            .map(prepared_account)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let imported = self
            .accounts
            .import_provider_accounts(ImportProviderAccounts {
                scope: ProviderAccountAdminScope {
                    provider_kind: provider_kind.clone(),
                },
                accounts,
                audit: mutation_audit(
                    context,
                    action,
                    "provider_account",
                    &provider_kind,
                    vec!["credentials".to_owned()],
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(CredentialImportResult {
            config_revision: admin_revision(imported.config_revision)?,
            credential_ids: imported
                .account_ids
                .into_iter()
                .map(CoreProviderAccountId::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::Unavailable,
                        ENTITY,
                        "provider account import returned an invalid account ID",
                    )
                })?,
        })
    }

    async fn commit_prepared_rotation(
        &self,
        prepared: PreparedCredentialRotationFacts,
        context: &MutationContext,
        action: &str,
    ) -> AdminStoreResult<CredentialMutationResult> {
        let account_id = prepared.account_id.clone();
        let scope = ProviderAccountAdminScope {
            provider_kind: prepared.provider_kind.as_str().to_owned(),
        };
        let rotation = self
            .accounts
            .rotate_provider_account(RotateProviderAccount {
                scope,
                profile: UpdateProviderAccount {
                    id: account_id.as_str().to_owned(),
                    name: prepared.name,
                    email: prepared.email,
                    plan_type: prepared.plan_type,
                },
                replacement_identity: prepared.replacement_identity,
                credential: ProviderCredentialUpdate {
                    account_id: account_id.as_str().to_owned(),
                    expected_revision: store_revision(prepared.expected_credential_revision)?,
                    provider_credentials_json: provider_document_json(prepared.provider_material)
                        .map_err(|error| admin_store_error(ENTITY, error))?,
                    has_refresh_token: prepared.has_refresh_token,
                    access_token_expires_at: prepared.access_token_expires_at,
                    next_refresh_at: prepared.next_refresh_at,
                },
                audit: mutation_audit(
                    context,
                    action,
                    "provider_account",
                    account_id.as_str(),
                    vec!["credentials".to_owned()],
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(CredentialMutationResult {
            config_revision: admin_revision(rotation.config_revision)?,
            account_id,
            credential_revision: Some(admin_revision(rotation.credential_revision)?),
        })
    }

    async fn account_groups_by_account(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<BTreeMap<String, Vec<AccountGroupRef>>> {
        if account_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows = sqlx::query_as::<_, (String, String, String, String, bool)>(
            "select m.provider_account_id, g.id, g.name, g.color, g.enabled
             from account_group_accounts m
             join account_groups g on g.id = m.account_group_id
             where m.provider_account_id = any($1::text[])
             order by m.provider_account_id, g.id",
        )
        .bind(account_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("load account group references"),
            )
        })?;
        let mut groups = BTreeMap::<String, Vec<AccountGroupRef>>::new();
        for (account_id, group_id, name, color, enabled) in rows {
            let id = AccountGroupId::new(group_id).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    ENTITY,
                    "persisted account group ID is invalid",
                )
            })?;
            groups.entry(account_id).or_default().push(AccountGroupRef {
                id,
                name,
                color: gateway_admin::model::account_groups::AccountGroupColor::parse(&color)
                    .ok_or_else(|| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Invalid,
                            ENTITY,
                            "persisted account group color is invalid",
                        )
                    })?,
                enabled,
            });
        }
        Ok(groups)
    }
}

#[async_trait]
impl AccountStore for PgAdminAccountStore {
    async fn list_accounts(
        &self,
        query: AdminAccountListQuery,
        runtime: gateway_admin::model::accounts::AccountRuntimeSnapshot,
    ) -> AdminStoreResult<AccountPage> {
        if query.page == 0 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "page number must be positive",
            ));
        }
        let now = Utc::now();
        let rate_limited_until = runtime
            .rate_limited_until
            .into_iter()
            .map(|(account_id, until)| (account_id, until.into()))
            .collect::<BTreeMap<_, _>>();
        let now_system_time = std::time::SystemTime::from(now);
        let active_rate_limited_ids = rate_limited_until
            .iter()
            .filter(|(_, until)| **until > now_system_time)
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>();
        let page =
            load_admin_account_page(&self.pool, &query, now, active_rate_limited_ids).await?;
        let item_ids = page
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let mut groups_by_account = self.account_groups_by_account(&item_ids).await?;
        let items = page
            .accounts
            .into_iter()
            .map(|summary| {
                let account_id = summary.id.clone();
                let projection = account_status_projection(
                    &summary,
                    now.into(),
                    rate_limited_until.get(&account_id).copied(),
                );
                let mut account = admin_account_record(summary)?;
                account.groups = groups_by_account.remove(&account_id).unwrap_or_default();
                Ok(AccountPageItem {
                    account,
                    projection,
                })
            })
            .collect::<AdminStoreResult<Vec<_>>>()?;
        Ok(AccountPage {
            config_revision: page.config_revision,
            items,
            total: page.total,
            summary: page.summary,
        })
    }

    async fn load_account(
        &self,
        account_id: &str,
        runtime: gateway_admin::model::accounts::AccountRuntimeSnapshot,
    ) -> AdminStoreResult<Option<AccountPageItem>> {
        let record = self
            .accounts
            .load_provider_account(account_id)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let Some(record) = record else {
            return Ok(None);
        };
        let now = Utc::now();
        let rate_limited_until = runtime
            .rate_limited_until
            .get(account_id)
            .copied()
            .map(Into::into);
        let projection = account_status_projection(&record.summary, now.into(), rate_limited_until);
        let account_id = record.summary.id.clone();
        let mut groups = self
            .account_groups_by_account(std::slice::from_ref(&account_id))
            .await?;
        let mut account = admin_account_record(record.summary)?;
        account.groups = groups.remove(&account_id).unwrap_or_default();
        Ok(Some(AccountPageItem {
            account,
            projection,
        }))
    }

    async fn load_account_usage(
        &self,
        range: TimeRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>> {
        let range = ObservabilityRange::new(range.start, range.end)
            .map_err(|error| admin_store_error(ENTITY, error))?;
        self.usage_observations(range, account_ids)
            .await?
            .into_iter()
            .map(admin_account_usage)
            .collect()
    }

    async fn load_account_usage_by_windows(
        &self,
        windows: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
        self.usage_by_windows(windows).await
    }

    async fn list_credentials(
        &self,
        provider_kind: &ProviderKind,
        query: CredentialListQuery,
    ) -> AdminStoreResult<CredentialPage> {
        let (control_plane, accounts) = futures::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts
                .list_provider_accounts(Some(provider_kind.as_str()), true),
        )
        .map_err(|error| admin_store_error(ENTITY, error))?;
        let mut accounts = accounts
            .into_iter()
            .filter(|account| account.provider_kind == provider_kind.as_str())
            .filter(|account| {
                query
                    .credential_state
                    .as_ref()
                    .is_none_or(|expected| expected.matches(account.credential_state))
            })
            .filter(|account| {
                query
                    .enabled
                    .is_none_or(|enabled| account.enabled == enabled)
            })
            .filter(|account| {
                let CredentialListWindow::Page {
                    cursor: Some(cursor),
                    ..
                } = &query.window
                else {
                    return true;
                };
                account.created_at > cursor.created_at
                    || (account.created_at == cursor.created_at
                        && account.id.as_str() > cursor.account_id.as_str())
            })
            .collect::<Vec<_>>();
        if matches!(&query.window, CredentialListWindow::Page { .. }) {
            accounts.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        let next_cursor = match query.window {
            CredentialListWindow::All => None,
            CredentialListWindow::Page { page_size, .. } => {
                let page_size = usize::from(page_size.get());
                let has_more = accounts.len() > page_size;
                accounts.truncate(page_size);
                has_more
                    .then(|| accounts.last())
                    .flatten()
                    .map(|account| {
                        Ok(CredentialCursor {
                            created_at: account.created_at,
                            account_id: CoreProviderAccountId::new(account.id.clone()).map_err(
                                |_| {
                                    AdminStoreError::new(
                                        AdminStoreErrorKind::Invalid,
                                        ENTITY,
                                        "persisted Provider account ID is invalid",
                                    )
                                },
                            )?,
                        })
                    })
                    .transpose()?
            }
        };
        Ok(CredentialPage {
            config_revision: admin_revision(control_plane.settings.config_revision)?,
            items: accounts
                .into_iter()
                .map(admin_account_record)
                .collect::<AdminStoreResult<Vec<_>>>()?,
            next_cursor,
        })
    }

    async fn credential_details(
        &self,
        provider_kind: &ProviderKind,
        account_id: &CoreProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>> {
        let (control_plane, account) = futures::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts.load_provider_account(account_id.as_str()),
        )
        .map_err(|error| admin_store_error(ENTITY, error))?;
        account
            .filter(|record| record.summary.provider_kind == provider_kind.as_str())
            .map(|record| {
                Ok(CredentialDetails {
                    config_revision: admin_revision(control_plane.settings.config_revision)?,
                    credential: admin_account_record(record.summary)?,
                })
            })
            .transpose()
    }

    async fn load_credentials_for_export(
        &self,
        provider_kind: &ProviderKind,
        account_ids: &[CoreProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>> {
        let ids = account_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_admin_account_ids(&ids).map_err(|error| admin_store_error(ENTITY, error))?;
        let mut credentials = Vec::with_capacity(account_ids.len());
        for account_id in account_ids {
            let record = self
                .accounts
                .load_provider_account(account_id.as_str())
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?
                .ok_or_else(|| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::NotFound,
                        ENTITY,
                        "one or more exported credentials do not exist",
                    )
                })?;
            if record.summary.provider_kind != provider_kind.as_str() {
                return Err(AdminStoreError::new(
                    AdminStoreErrorKind::NotFound,
                    ENTITY,
                    "one or more exported credentials belong to another Provider",
                ));
            }
            credentials.push(ProviderExportCredentialInput {
                account: admin_account_record(record.summary)?,
                provider_material: ProviderDocument::new(OpaqueProviderData::new(
                    record.provider_credentials_json.fields().clone(),
                )),
            });
        }
        Ok(credentials)
    }

    async fn commit_credential_import(
        &self,
        command: CredentialImportCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialImportResult> {
        self.commit_prepared_import(command.prepared, context, "import_document")
            .await
    }

    async fn commit_authorization(
        &self,
        command: AuthorizationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        match command.credential {
            AuthorizationCredentialCommit::Create(credential) => {
                let CredentialImportResult {
                    config_revision,
                    credential_ids,
                } = self
                    .commit_prepared_import(
                        PreparedCredentialImport {
                            provider_kind: credential.provider_kind.clone(),
                            credentials: vec![credential],
                        },
                        context,
                        "authorize",
                    )
                    .await?;
                let [account_id]: [CoreProviderAccountId; 1] =
                    credential_ids.try_into().map_err(|_| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Unavailable,
                            ENTITY,
                            "authorization import returned an unexpected account count",
                        )
                    })?;
                let details = self
                    .accounts
                    .load_provider_account(account_id.as_str())
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?
                    .ok_or_else(|| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Unavailable,
                            ENTITY,
                            "authorized credential was not visible after commit",
                        )
                    })?;
                Ok(CredentialMutationResult {
                    config_revision,
                    account_id,
                    credential_revision: Some(admin_revision(details.summary.credential_revision)?),
                })
            }
            AuthorizationCredentialCommit::Reauthorize(prepared) => {
                self.commit_prepared_rotation(prepared, context, "reauthorize")
                    .await
            }
        }
    }

    async fn commit_credential_rotation(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.commit_prepared_rotation(command.prepared, context, "rotate_credential")
            .await
    }

    async fn commit_credential_refresh(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.commit_prepared_rotation(command.prepared, context, "refresh_credential")
            .await
    }

    async fn update_account(
        &self,
        command: UpdateAccount,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        let account_id = CoreProviderAccountId::new(command.account_id.clone()).map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "invalid provider account ID",
            )
        })?;
        let config_revision = self
            .accounts
            .batch_update_provider_accounts_admin(BatchUpdateProviderAccountsAdmin {
                account_ids: vec![command.account_id.clone()],
                enabled: command.enabled,
                concurrency_limit: command.concurrency_limit,
                weight: command.weight,
                group_ids: command.group_ids,
                audit: mutation_audit(
                    context,
                    "update",
                    "provider_account",
                    &command.account_id,
                    vec![
                        "enabled".to_owned(),
                        "concurrency_limit".to_owned(),
                        "weight".to_owned(),
                        "groups".to_owned(),
                    ],
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)?;
        Ok(AccountUpdateResult {
            config_revision,
            account_id,
        })
    }

    async fn recover_account(
        &self,
        account_id: &CoreProviderAccountId,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        if let Some(cooldowns) = self.cooldowns.as_deref() {
            cooldowns.clear_all(account_id).await.map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Unavailable,
                    ENTITY,
                    "provider account cooldown cleanup failed",
                )
            })?;
        }
        let config_revision = self
            .accounts
            .recover_provider_account_admin(RecoverProviderAccount {
                account_id: account_id.as_str().to_owned(),
                audit: mutation_audit(
                    context,
                    "recover",
                    "provider_account",
                    account_id.as_str(),
                    vec!["status".to_owned(), "quota".to_owned()],
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)?;
        Ok(AccountUpdateResult {
            config_revision,
            account_id: account_id.clone(),
        })
    }

    async fn batch_update_accounts(
        &self,
        command: BatchUpdateAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountsUpdateResult> {
        let account_ids = command
            .account_ids
            .iter()
            .map(|id| {
                CoreProviderAccountId::new(id.clone()).map_err(|_| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::Invalid,
                        ENTITY,
                        "invalid provider account ID",
                    )
                })
            })
            .collect::<AdminStoreResult<Vec<_>>>()?;
        let audit_target = if command.account_ids.len() == 1 {
            command.account_ids[0].clone()
        } else {
            "provider_accounts".to_owned()
        };
        let config_revision = self
            .accounts
            .batch_update_provider_accounts_admin(BatchUpdateProviderAccountsAdmin {
                account_ids: command.account_ids,
                enabled: command.enabled,
                concurrency_limit: command.concurrency_limit,
                weight: command.weight,
                group_ids: command.group_ids,
                audit: mutation_audit(
                    context,
                    "batch_update",
                    "provider_account",
                    &audit_target,
                    vec![
                        "enabled".to_owned(),
                        "concurrency_limit".to_owned(),
                        "weight".to_owned(),
                        "groups".to_owned(),
                    ],
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)?;
        Ok(AccountsUpdateResult {
            config_revision,
            account_ids,
        })
    }

    async fn delete_accounts(
        &self,
        command: DeleteAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let first_account_id = command.account_ids.first().ok_or_else(|| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "account deletion requires at least one account ID",
            )
        })?;
        let scope = self.required_scope(first_account_id).await?;
        let audit_target = if command.account_ids.len() == 1 {
            first_account_id.clone()
        } else {
            "provider_accounts".to_owned()
        };
        self.accounts
            .delete_provider_accounts_admin(DeleteProviderAccounts {
                scope,
                account_ids: command.account_ids,
                audit: mutation_audit(
                    context,
                    "delete",
                    "provider_account",
                    &audit_target,
                    Vec::new(),
                ),
            })
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)
    }

    async fn record_credential_export(
        &self,
        account_ids: &[CoreProviderAccountId],
        context: &MutationContext,
    ) -> AdminStoreResult<()> {
        let ids = account_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_admin_account_ids(&ids).map_err(|error| admin_store_error(ENTITY, error))?;
        for account_id in &ids {
            if self
                .accounts
                .load_provider_account(account_id)
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?
                .is_none()
            {
                return Err(AdminStoreError::new(
                    AdminStoreErrorKind::NotFound,
                    ENTITY,
                    "one or more exported credentials do not exist",
                ));
            }
        }
        let control_plane = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let revision = control_plane.settings.config_revision;
        let mut transaction = self.accounts.pool.begin().await.map_err(|_| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("begin credential export audit"),
            )
        })?;
        let result = async {
            for account_id in &ids {
                append_admin_audit_event_in_transaction(
                    &mut transaction,
                    mutation_audit(
                        context,
                        "export_credentials",
                        "provider_account",
                        account_id,
                        Vec::new(),
                    ),
                    revision,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        finish_admin_transaction(transaction, result, "credential export audit")
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
    }
}
