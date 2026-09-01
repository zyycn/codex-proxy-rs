use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::{
    account::{
        AccountStatusFacts, CredentialState, OpaqueProviderData, ProviderAccountId, QuotaEvidence,
        QuotaState, resolve_account_status,
    },
    engine::probe::{
        AccountProbe, AccountProbeError, AccountProbeErrorSource, AccountProbeRequest,
        AccountProbeResult,
    },
    error::{ClientVisibleUpstreamError, GatewayError, GatewayErrorKind},
    operation::{GenerateRequest, Operation, ProtocolPayload},
    routing::ProviderKind,
    upstream::UpstreamSendState,
};

use gateway_admin::{
    AdminServices,
    model::{
        AdminError, MutationContext, Revision,
        accounts::{
            AccountConnectionTestEvent, AccountListQuery, AccountPage, AccountPageItem,
            AccountRecord, AccountRuntimeSnapshot, AccountSummary, AccountUpdateResult,
            AccountUsage, AccountUsageWindowQuery, AccountUsageWindowResult, AccountsUpdateResult,
            BatchUpdateAccounts, DeleteAccounts, UpdateAccount,
        },
        observability::TimeRange,
        provider_credentials::{
            AuthorizationCommit, AuthorizationCommitGuard, AuthorizationCredentialCommit,
            AuthorizationMutationTarget, AuthorizationStarted, CompleteAuthorization,
            ConsumeProviderResetCredit, CredentialCommitGuard, CredentialDetails,
            CredentialImportCommit, CredentialImportResult, CredentialListQuery,
            CredentialMutationResult, CredentialPage, CredentialRotationCommit,
            PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
            PrepareCredentialRotation, PreparedAuthorizationCommit,
            PreparedAuthorizationCredential, PreparedCredentialCreate, PreparedCredentialImport,
            PreparedCredentialRotation, PreparedCredentialRotationFacts, ProviderDocument,
            ProviderExport, ProviderExportCredentialInput, ProviderModels, ProviderQuota,
            ProviderQuotaRequest, ProviderQuotaWindow, ProviderResetCreditResult,
            QuotaLocalUsageAttribution,
        },
        settings::{
            AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RotationStrategy,
            RuntimeSettings,
        },
    },
    ports::{
        provider::{
            ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind, ProviderAdminRegistry,
        },
        store::{
            AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStoreResult, SettingsStore,
        },
    },
};
use serde_json::{Map, json};

pub(super) type EventLog = Arc<Mutex<Vec<&'static str>>>;

pub(super) struct FakeProviderAdmin {
    kind: ProviderKind,
    events: EventLog,
    failure: Mutex<Option<ProviderAdminError>>,
    quota_failure: Mutex<Option<ProviderAdminErrorKind>>,
    pending: Arc<Mutex<Option<PendingAuthorizationMutation>>>,
    retry_authorization_after_abort: Mutex<bool>,
    export_inputs: Mutex<Vec<ProviderExportCredentialInput>>,
    import_account_ids: Mutex<Vec<String>>,
    quota_requests: Mutex<Vec<ProviderQuotaRequest>>,
    quota: Mutex<ProviderQuota>,
    current_credential_revision: Mutex<Revision>,
    reset_credit_commands: Mutex<Vec<ConsumeProviderResetCredit>>,
}

impl FakeProviderAdmin {
    pub(super) fn new(kind: &str, events: EventLog) -> Arc<Self> {
        Arc::new(Self {
            kind: ProviderKind::new(kind).expect("provider kind"),
            events,
            failure: Mutex::new(None),
            quota_failure: Mutex::new(None),
            pending: Arc::new(Mutex::new(None)),
            retry_authorization_after_abort: Mutex::new(false),
            export_inputs: Mutex::new(Vec::new()),
            import_account_ids: Mutex::new(vec!["acct_prepared".to_owned()]),
            quota_requests: Mutex::new(Vec::new()),
            quota: Mutex::new(empty_quota()),
            current_credential_revision: Mutex::new(revision(1)),
            reset_credit_commands: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn fail_next(&self, kind: ProviderAdminErrorKind) {
        *self.failure.lock().expect("provider failure") = Some(ProviderAdminError::new(kind));
    }

    fn fail_next_with_message(&self, kind: ProviderAdminErrorKind, message: &str) {
        *self.failure.lock().expect("provider failure") =
            Some(ProviderAdminError::new(kind).with_message(message));
    }

    pub(super) fn fail_next_quota(&self, kind: ProviderAdminErrorKind) {
        *self.quota_failure.lock().expect("provider quota failure") = Some(kind);
    }

    pub(super) fn set_import_account_ids(&self, account_ids: &[&str]) {
        *self
            .import_account_ids
            .lock()
            .expect("provider import account IDs") = account_ids
            .iter()
            .map(|account_id| (*account_id).to_owned())
            .collect();
    }

    pub(super) fn quota_requests(&self) -> Vec<ProviderQuotaRequest> {
        self.quota_requests
            .lock()
            .expect("provider quota requests")
            .clone()
    }

    fn reset_credit_commands(&self) -> Vec<ConsumeProviderResetCredit> {
        self.reset_credit_commands
            .lock()
            .expect("provider reset-credit commands")
            .clone()
    }

    pub(super) fn set_quota(&self, quota: ProviderQuota) {
        *self.quota.lock().expect("provider quota") = quota;
    }

    pub(super) fn pending(&self) -> Option<PendingAuthorizationMutation> {
        self.pending.lock().expect("pending authorization").clone()
    }

    pub(super) fn retry_authorization_after_abort(&self) {
        *self
            .retry_authorization_after_abort
            .lock()
            .expect("authorization retry") = true;
    }

    pub(super) fn set_current_credential_revision(&self, revision: Revision) {
        *self
            .current_credential_revision
            .lock()
            .expect("current credential revision") = revision;
    }

    fn export_inputs(&self) -> Vec<ProviderExportCredentialInput> {
        self.export_inputs.lock().expect("export inputs").clone()
    }

    fn record(&self, event: &'static str) {
        self.events.lock().expect("provider events").push(event);
    }

    fn require_available(&self) -> Result<(), ProviderAdminError> {
        match self.failure.lock().expect("provider failure").take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn prepared_rotation(&self, account: &AccountRecord) -> PreparedCredentialRotation {
        PreparedCredentialRotation::new(
            PreparedCredentialRotationFacts {
                account_id: ProviderAccountId::new(account.id.clone()).expect("account ID"),
                provider_kind: account.provider_kind.clone(),
                expected_credential_revision: *self
                    .current_credential_revision
                    .lock()
                    .expect("current credential revision"),
                replacement_identity: None,
                name: account.name.clone(),
                email: account.email.clone(),
                plan_type: account.plan_type.clone(),
                provider_material: document(),
                has_refresh_token: account.has_refresh_token,
                access_token_expires_at: account
                    .access_token_expires_at
                    .map(|expires_at| expires_at + TimeDelta::hours(1)),
                next_refresh_at: account.next_refresh_at,
            },
            Box::new(RecordingGuard::new(self.events.clone())),
        )
    }
}

#[async_trait]
impl ProviderAdmin for FakeProviderAdmin {
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }

    async fn account_unavailable(&self, _: &ProviderAccountId) {
        self.record("provider.account_unavailable");
    }

    async fn account_facts_changed(&self, _: &[ProviderAccountId]) {
        self.record("provider.account_facts_changed");
    }

    fn connection_test_operation(
        &self,
        model: &gateway_core::routing::UpstreamModelId,
        input: &str,
    ) -> Result<gateway_core::operation::Operation, ProviderAdminError> {
        let payload = ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!(model.as_str())),
                ("input".to_owned(), json!(input)),
                ("stream".to_owned(), json!(true)),
                ("store".to_owned(), json!(false)),
            ]),
        )
        .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::Invalid))?;
        Ok(Operation::Generate(GenerateRequest::from_protocol_payload(
            payload,
        )))
    }

    fn dashboard_wire_profile(
        &self,
    ) -> Option<gateway_admin::model::observability::DashboardWireProfile> {
        None
    }

    fn calculated_billing(
        &self,
        _: &gateway_admin::model::observability::ProviderBillingInput,
    ) -> Result<
        Option<gateway_admin::model::observability::CalculatedBillingBreakdown>,
        ProviderAdminError,
    > {
        Ok(None)
    }

    async fn prepare_import(
        &self,
        _command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        self.record("provider.prepare_import");
        self.require_available()?;
        let account_ids = self
            .import_account_ids
            .lock()
            .expect("provider import account IDs")
            .clone();
        Ok(PreparedCredentialImport {
            provider_kind: self.kind.clone(),
            credentials: account_ids
                .into_iter()
                .map(|account_id| {
                    prepared_create_with_id(self.kind.clone(), &account_id, "prepared-import")
                })
                .collect(),
        })
    }

    async fn start_authorization(
        &self,
        pending: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        self.record("provider.start_authorization");
        self.require_available()?;
        *self.pending.lock().expect("pending authorization") = Some(pending);
        Ok(AuthorizationStarted {
            flow_id: "flow-test".to_owned(),
            authorization_url: "https://example.invalid/oauth".to_owned(),
            expires_at: Utc::now() + TimeDelta::minutes(10),
        })
    }

    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        self.record("provider.complete_authorization");
        self.require_available()?;
        let pending = self
            .pending
            .lock()
            .expect("pending authorization")
            .take()
            .ok_or_else(unsupported)?;
        if !pending.owner_binding().matches_context(&command.context) {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::NotFound));
        }
        let retry_pending = (*self
            .retry_authorization_after_abort
            .lock()
            .expect("authorization retry"))
        .then(|| pending.clone());
        let credential = match pending.target() {
            AuthorizationMutationTarget::Create { name } => {
                PreparedAuthorizationCredential::Create(prepared_create(self.kind.clone(), name))
            }
            AuthorizationMutationTarget::Reauthorize { account_id } => {
                let mut account = account_record(self.kind.as_str());
                account.id = account_id.as_str().to_owned();
                PreparedAuthorizationCredential::Reauthorize(self.prepared_rotation(&account))
            }
        };
        let prepared = PreparedAuthorizationCommit::new(pending, credential);
        Ok(match retry_pending {
            Some(pending) => {
                prepared.with_authorization_guard(Box::new(RetryableAuthorizationGuard::new(
                    self.events.clone(),
                    Arc::clone(&self.pending),
                    pending,
                )))
            }
            None => prepared,
        })
    }

    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        self.record("provider.prepare_rotation");
        self.require_available()?;
        Ok(self.prepared_rotation(&command.account))
    }

    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        self.record("provider.prepare_refresh");
        self.require_available()?;
        Ok(self.prepared_rotation(&command.account))
    }

    async fn quota(
        &self,
        request: ProviderQuotaRequest,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        if request.refresh {
            self.record("provider.quota");
        }
        self.quota_requests
            .lock()
            .expect("provider quota requests")
            .push(request);
        if let Some(kind) = self
            .quota_failure
            .lock()
            .expect("provider quota failure")
            .take()
        {
            return Err(ProviderAdminError::new(kind));
        }
        Ok(self.quota.lock().expect("provider quota").clone())
    }

    async fn consume_reset_credit(
        &self,
        command: ConsumeProviderResetCredit,
    ) -> Result<ProviderResetCreditResult, ProviderAdminError> {
        self.record("provider.consume_reset_credit");
        self.reset_credit_commands
            .lock()
            .expect("provider reset-credit commands")
            .push(command);
        self.require_available()?;
        Ok(ProviderResetCreditResult {
            code: "reset".to_owned(),
            credit: None,
        })
    }

    async fn models(
        &self,
        _: &ProviderAccountId,
        _: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        Ok(ProviderModels {
            models: Vec::new(),
            observed_at: None,
        })
    }

    async fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        self.record("provider.export");
        self.require_available()?;
        *self.export_inputs.lock().expect("export inputs") = credentials.clone();
        Ok(ProviderExport {
            provider_kind: self.kind.clone(),
            account_ids: credentials
                .into_iter()
                .map(|credential| {
                    ProviderAccountId::new(credential.account.id).expect("stored account ID")
                })
                .collect(),
            document: document(),
        })
    }
}

pub(super) struct FakeAccountStore {
    events: EventLog,
    accounts: Mutex<Vec<AccountRecord>>,
    account_after_probe: Mutex<Option<AccountRecord>>,
    fail_commit: Mutex<bool>,
    audit_requests: Mutex<Vec<String>>,
    quota_window_usage: Mutex<Vec<AccountUsageWindowResult>>,
    quota_window_queries: Mutex<Vec<AccountUsageWindowQuery>>,
}

impl FakeAccountStore {
    pub(super) fn new(kind: &str, events: EventLog) -> Arc<Self> {
        Self::with_account(account_record(kind), events)
    }

    fn with_account(account: AccountRecord, events: EventLog) -> Arc<Self> {
        Arc::new(Self {
            events,
            accounts: Mutex::new(vec![account]),
            account_after_probe: Mutex::new(None),
            fail_commit: Mutex::new(false),
            audit_requests: Mutex::new(Vec::new()),
            quota_window_usage: Mutex::new(Vec::new()),
            quota_window_queries: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn fail_next_commit(&self) {
        *self.fail_commit.lock().expect("store failure") = true;
    }

    pub(super) fn audit_requests(&self) -> Vec<String> {
        self.audit_requests.lock().expect("audit requests").clone()
    }

    pub(super) fn set_quota_window_usage(&self, usage: Vec<AccountUsageWindowResult>) {
        *self.quota_window_usage.lock().expect("quota window usage") = usage;
    }

    fn quota_window_queries(&self) -> Vec<AccountUsageWindowQuery> {
        self.quota_window_queries
            .lock()
            .expect("quota window queries")
            .clone()
    }

    pub(super) fn set_accounts(&self, accounts: Vec<AccountRecord>) {
        *self.accounts.lock().expect("accounts") = accounts;
    }

    fn set_account_after_probe(&self, account: AccountRecord) {
        *self
            .account_after_probe
            .lock()
            .expect("account after probe") = Some(account);
    }

    fn record(&self, event: &'static str) {
        self.events.lock().expect("store events").push(event);
    }

    fn require_commit(&self) -> AdminStoreResult<()> {
        let mut failure = self.fail_commit.lock().expect("store failure");
        if std::mem::take(&mut *failure) {
            Err(store_unavailable())
        } else {
            Ok(())
        }
    }

    fn record_context(&self, context: &MutationContext) {
        self.audit_requests
            .lock()
            .expect("audit requests")
            .push(context.request_id.clone());
    }

    fn page_item(account: AccountRecord) -> AccountPageItem {
        let facts = AccountStatusFacts {
            enabled: account.enabled,
            credential_state: account.credential_state,
            access_token_expires_at: account.access_token_expires_at.map(Into::into),
            quota: account.quota,
            rate_limited_until: None,
            last_error_reason: account.last_error_reason,
            last_error_message: account.last_error_message.clone(),
        };
        AccountPageItem {
            account,
            projection: resolve_account_status(&facts, std::time::SystemTime::now()),
        }
    }
}

#[async_trait]
impl AccountStore for FakeAccountStore {
    async fn list_accounts(
        &self,
        _: AccountListQuery,
        _: AccountRuntimeSnapshot,
    ) -> AdminStoreResult<AccountPage> {
        self.record("store.list_accounts");
        let accounts = self.accounts.lock().expect("accounts").clone();
        let total = accounts.len() as u64;
        Ok(AccountPage {
            config_revision: revision(1),
            items: accounts.into_iter().map(Self::page_item).collect(),
            total,
            summary: AccountSummary {
                total,
                normal: 1,
                quota_exhausted: 0,
                rate_limited: 0,
                disabled: 0,
                error: 0,
            },
        })
    }

    async fn load_account(
        &self,
        account_id: &str,
        _: AccountRuntimeSnapshot,
    ) -> AdminStoreResult<Option<AccountPageItem>> {
        self.record("store.load_account");
        // probe 后的账号状态覆盖只对同一 id 生效；其余按账号列表查询。
        let account = self
            .account_after_probe
            .lock()
            .expect("account after probe")
            .clone()
            .filter(|account| account.id == account_id)
            .or_else(|| {
                self.accounts
                    .lock()
                    .expect("accounts")
                    .iter()
                    .find(|account| account.id == account_id)
                    .cloned()
            });
        Ok(account.map(Self::page_item))
    }

    async fn load_account_usage(
        &self,
        _: TimeRange,
        _: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>> {
        Ok(Vec::new())
    }

    async fn load_account_usage_by_windows(
        &self,
        windows: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
        *self
            .quota_window_queries
            .lock()
            .expect("quota window queries") = windows.to_vec();
        Ok(self
            .quota_window_usage
            .lock()
            .expect("quota window usage")
            .clone())
    }

    async fn list_credentials(
        &self,
        provider_kind: &ProviderKind,
        _: CredentialListQuery,
    ) -> AdminStoreResult<CredentialPage> {
        self.record("store.list_credentials");
        let accounts = self.accounts.lock().expect("accounts").clone();
        Ok(CredentialPage {
            config_revision: revision(1),
            items: accounts
                .into_iter()
                .filter(|account| &account.provider_kind == provider_kind)
                .collect(),
            next_cursor: None,
        })
    }

    async fn credential_details(
        &self,
        provider_kind: &ProviderKind,
        account_id: &ProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>> {
        self.record("store.credential_details");
        let account = self
            .accounts
            .lock()
            .expect("accounts")
            .iter()
            .find(|account| {
                &account.provider_kind == provider_kind && account.id == account_id.as_str()
            })
            .cloned();
        Ok(account.map(|credential| CredentialDetails {
            config_revision: revision(1),
            credential,
        }))
    }

    async fn load_credentials_for_export(
        &self,
        provider_kind: &ProviderKind,
        account_ids: &[ProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>> {
        self.record("store.load_credentials_for_export");
        let accounts = self.accounts.lock().expect("accounts").clone();
        if accounts.iter().any(|account| {
            &account.provider_kind != provider_kind
                || !account_ids
                    .iter()
                    .any(|account_id| account_id.as_str() == account.id)
        }) {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::NotFound,
                "test account",
                "credential not found",
            ));
        }
        Ok(accounts
            .into_iter()
            .map(|account| ProviderExportCredentialInput {
                account,
                provider_material: document(),
            })
            .collect())
    }

    async fn commit_credential_import(
        &self,
        command: CredentialImportCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialImportResult> {
        self.record("store.commit_import");
        self.record_context(context);
        self.require_commit()?;
        Ok(CredentialImportResult {
            config_revision: revision(2),
            credential_ids: command
                .prepared
                .credentials
                .into_iter()
                .map(|credential| credential.account_id)
                .collect(),
        })
    }

    async fn commit_authorization(
        &self,
        command: AuthorizationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.record("store.commit_authorization");
        self.record_context(context);
        self.require_commit()?;
        let (account_id, credential_revision) = match command.credential {
            AuthorizationCredentialCommit::Create(credential) => (credential.account_id, None),
            AuthorizationCredentialCommit::Reauthorize(credential) => (
                credential.account_id,
                Some(revision(credential.expected_credential_revision.get() + 1)),
            ),
        };
        Ok(CredentialMutationResult {
            config_revision: revision(2),
            account_id,
            credential_revision,
        })
    }

    async fn commit_credential_rotation(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.record("store.commit_rotation");
        self.record_context(context);
        self.require_commit()?;
        Ok(rotation_result(command))
    }

    async fn commit_credential_refresh(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.record("store.commit_refresh");
        self.record_context(context);
        self.require_commit()?;
        Ok(rotation_result(command))
    }

    async fn update_account(
        &self,
        command: UpdateAccount,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        self.record("store.update_account");
        self.record_context(context);
        self.require_commit()?;
        Ok(AccountUpdateResult {
            config_revision: revision(2),
            account_id: ProviderAccountId::new(command.account_id).expect("account ID"),
        })
    }

    async fn recover_account(
        &self,
        account_id: &ProviderAccountId,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        self.record("store.recover_account");
        self.record_context(context);
        self.require_commit()?;
        let mut accounts = self.accounts.lock().expect("accounts");
        if let Some(account) = accounts
            .iter_mut()
            .find(|account| account.id == account_id.as_str())
        {
            account.enabled = true;
            account.credential_state = CredentialState::Ready;
            account.quota = QuotaState::allowed(std::time::SystemTime::now());
            account.last_error_reason = None;
            account.last_error_message = None;
        }
        Ok(AccountUpdateResult {
            config_revision: revision(2),
            account_id: account_id.clone(),
        })
    }

    async fn batch_update_accounts(
        &self,
        command: BatchUpdateAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountsUpdateResult> {
        self.record("store.batch_update_accounts");
        self.record_context(context);
        self.require_commit()?;
        Ok(AccountsUpdateResult {
            config_revision: revision(2),
            account_ids: command
                .account_ids
                .into_iter()
                .map(|id| ProviderAccountId::new(id).expect("account ID"))
                .collect(),
        })
    }

    async fn delete_accounts(
        &self,
        _: DeleteAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        self.record("store.delete");
        self.record_context(context);
        self.require_commit()?;
        Ok(revision(2))
    }

    async fn record_credential_export(
        &self,
        _: &[ProviderAccountId],
        context: &MutationContext,
    ) -> AdminStoreResult<()> {
        self.record("store.audit_export");
        self.record_context(context);
        Ok(())
    }
}

struct StaticSettingsStore;

#[async_trait]
impl SettingsStore for StaticSettingsStore {
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Ok(RuntimeSettings {
            config_revision: revision(1),
            model_mappings: Default::default(),
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
            max_concurrent_per_account: 1,
            request_interval_ms: 0,
            rotation_strategy: RotationStrategy::Smart,
            min_codex_desktop_version: None,
            min_codex_cli_version: None,
            usage_retention_days: 30,
            ops_event_retention_days: 30,
            audit_retention_days: 30,
            updated_at: Utc::now(),
        })
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Err(store_unavailable())
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(store_unavailable())
    }

    async fn replace_admin_api_key(
        &self,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(store_unavailable())
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(store_unavailable())
    }
}

struct RecordingGuard {
    events: EventLog,
    finished: bool,
}

impl RecordingGuard {
    fn new(events: EventLog) -> Self {
        Self {
            events,
            finished: false,
        }
    }
}

impl CredentialCommitGuard for RecordingGuard {
    fn finish(mut self: Box<Self>) {
        self.events
            .lock()
            .expect("guard events")
            .push("guard.finish");
        self.finished = true;
    }
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.events.lock().expect("guard events").push("guard.drop");
        }
    }
}

struct RetryableAuthorizationGuard {
    events: EventLog,
    pending: Arc<Mutex<Option<PendingAuthorizationMutation>>>,
    authorization: PendingAuthorizationMutation,
}

impl RetryableAuthorizationGuard {
    fn new(
        events: EventLog,
        pending: Arc<Mutex<Option<PendingAuthorizationMutation>>>,
        authorization: PendingAuthorizationMutation,
    ) -> Self {
        Self {
            events,
            pending,
            authorization,
        }
    }
}

#[async_trait]
impl AuthorizationCommitGuard for RetryableAuthorizationGuard {
    async fn commit(self: Box<Self>) -> Result<(), AdminError> {
        self.events
            .lock()
            .expect("authorization guard events")
            .push("authorization_guard.commit");
        Ok(())
    }

    async fn abort(self: Box<Self>) -> Result<(), AdminError> {
        let Self {
            events,
            pending,
            authorization,
        } = *self;
        *pending.lock().expect("pending authorization") = Some(authorization);
        events
            .lock()
            .expect("authorization guard events")
            .push("authorization_guard.abort");
        Ok(())
    }
}

#[test]
fn provider_registry_should_resolve_custom_kind_without_central_match() {
    let provider = FakeProviderAdmin::new("custom-provider", events());
    let registry = ProviderAdminRegistry::new([provider as Arc<dyn ProviderAdmin>])
        .expect("provider registry");
    let resolved = registry
        .require(&ProviderKind::new("custom-provider").expect("provider kind"))
        .expect("registered provider");
    assert_eq!(resolved.provider_kind().as_str(), "custom-provider");
}

#[test]
fn provider_registry_should_reject_duplicate_kind() {
    let first = FakeProviderAdmin::new("duplicate", events());
    let second = FakeProviderAdmin::new("duplicate", events());
    let result = ProviderAdminRegistry::new([
        first as Arc<dyn ProviderAdmin>,
        second as Arc<dyn ProviderAdmin>,
    ]);
    assert!(matches!(
        result,
        Err(error) if error.kind() == ProviderAdminErrorKind::Conflict
    ));
}

#[tokio::test]
async fn connection_test_should_probe_unavailable_account() {
    let provider = FakeProviderAdmin::new("xai", events());
    let mut account = account_record("xai");
    account.quota = QuotaState::exhausted(
        QuotaEvidence::UsageLimitReached,
        std::time::SystemTime::now(),
        None,
    );
    let store = FakeAccountStore::with_account(account, events());
    let services =
        accounts_service_with_probe(provider, store, Arc::new(FailingAccountProbe)).await;

    let events = services
        .accounts()
        .test_connection(
            ProviderAccountId::new("acct_test").expect("account ID"),
            gateway_core::routing::UpstreamModelId::new("grok-4.5").expect("model"),
        )
        .await
        .expect("connection test stream")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.last(),
        Some(AccountConnectionTestEvent::Failed {
            source: AccountProbeErrorSource::Upstream,
            gateway_error_code: GatewayErrorKind::RateLimited,
            send_state: Some(UpstreamSendState::NotSent),
            message,
            provider_error_code: Some(code),
            provider_error_type: Some(error_type),
            ..
        }) if message == "included usage exhausted"
            && code == "usage_exhausted"
            && error_type == "invalid_request_error"
    ));
}

#[tokio::test]
async fn connection_test_rate_limited_probe_returns_provider_failure() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    let store = FakeAccountStore::new("xai", events);
    let probe = Arc::new(FreeModelQuotaProbe {
        store: Arc::clone(&store),
    });
    let services = accounts_service_with_probe(provider, store, probe).await;

    let events = services
        .accounts()
        .test_connection(
            ProviderAccountId::new("acct_test").expect("account ID"),
            gateway_core::routing::UpstreamModelId::new("grok-4.5").expect("model"),
        )
        .await
        .expect("connection test stream")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.last(),
        Some(AccountConnectionTestEvent::Failed {
            source: AccountProbeErrorSource::Provider,
            gateway_error_code: GatewayErrorKind::RateLimited,
            send_state: Some(UpstreamSendState::NotSent),
            message,
            ..
        })
        if message == "xAI free model quota is exhausted"
    ));
}

#[tokio::test]
async fn connection_test_should_preserve_disabled_account_status() {
    let provider = FakeProviderAdmin::new("xai", events());
    let mut account = account_record("xai");
    account.enabled = false;
    let store = FakeAccountStore::with_account(account, events());
    let services =
        accounts_service_with_probe(provider, store, Arc::new(FailingAccountProbe)).await;

    let events = services
        .accounts()
        .test_connection(
            ProviderAccountId::new("acct_test").expect("account ID"),
            gateway_core::routing::UpstreamModelId::new("grok-4.5").expect("model"),
        )
        .await
        .expect("connection test stream")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.last(),
        Some(AccountConnectionTestEvent::Failed { .. })
    ));
}

#[tokio::test]
async fn accounts_export_should_pass_store_loaded_timestamps_and_material_to_provider() {
    let provider = FakeProviderAdmin::new("openai", events());
    let store = FakeAccountStore::new("openai", events());
    let expected = store.accounts.lock().expect("accounts")[0].clone();
    accounts_service(provider.clone(), store)
        .await
        .accounts()
        .export(
            &context("export-complete-input"),
            vec![ProviderAccountId::new(expected.id.clone()).expect("account ID")],
        )
        .await
        .expect("export credentials");

    let inputs = provider.export_inputs();
    let input = inputs.first().expect("provider export input");
    assert_eq!(
        (
            input.account.created_at,
            input.account.updated_at,
            &input.provider_material,
        ),
        (expected.created_at, expected.updated_at, &document()),
    );
}

#[tokio::test]
async fn accounts_refresh_should_keep_guard_through_store_commit() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = accounts_service(provider, store.clone()).await;

    let result = services
        .accounts()
        .refresh(
            &context("refresh-request"),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect("refresh credential");
    assert_eq!(result.config_revision, revision(2));
    assert_eq!(result.account.account.provider_kind.as_str(), "openai");
    assert_eq!(
        result.account.projection.status,
        gateway_admin::model::accounts::AccountStatus::Normal
    );

    assert_eq!(
        recorded(&events),
        [
            "store.load_account",
            "provider.prepare_refresh",
            "store.commit_refresh",
            "guard.finish",
            "provider.account_facts_changed",
            "store.load_account",
        ]
    );
    assert_eq!(store.audit_requests(), ["refresh-request"]);
}

#[tokio::test]
async fn accounts_recover_should_commit_facts_then_return_normal_account() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let mut account = account_record("openai");
    account.enabled = false;
    account.credential_state = CredentialState::Invalid;
    account.quota = QuotaState::exhausted(
        QuotaEvidence::UsageLimitReached,
        std::time::SystemTime::now(),
        None,
    );
    account.last_error_reason = Some(gateway_core::account::AccountErrorReason::CredentialInvalid);
    account.last_error_message = Some("invalid credential".to_owned());
    let store = FakeAccountStore::with_account(account, events.clone());
    let services = accounts_service(provider, store.clone()).await;

    let result = services
        .accounts()
        .recover(
            &context("recover-request"),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect("recover account");

    assert_eq!(result.config_revision, revision(2));
    assert!(result.account.account.enabled);
    assert_eq!(
        result.account.account.credential_state,
        CredentialState::Ready
    );
    assert_eq!(
        result.account.account.quota.access(),
        gateway_core::account::QuotaAccessState::Allowed
    );
    assert_eq!(
        result.account.projection.status,
        gateway_admin::model::accounts::AccountStatus::Normal
    );
    assert_eq!(
        recorded(&events),
        [
            "store.load_account",
            "store.recover_account",
            "provider.account_facts_changed",
            "store.load_account",
        ]
    );
    assert_eq!(store.audit_requests(), ["recover-request"]);
}

#[tokio::test]
async fn accounts_update_should_commit_then_release_disabled_account_and_publish_facts() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = accounts_service(provider, store.clone()).await;

    let result = services
        .accounts()
        .update(
            &context("update-request"),
            UpdateAccount {
                account_id: "acct_test".to_owned(),
                enabled: false,
                concurrency_limit: None,
                weight: gateway_core::account::AccountWeight::DEFAULT,
                group_ids: Vec::new(),
            },
        )
        .await
        .expect("update account");

    assert_eq!(result.config_revision, revision(2));
    assert_eq!(
        recorded(&events),
        [
            "store.load_account",
            "store.update_account",
            "provider.account_unavailable",
            "provider.account_facts_changed",
        ]
    );
    assert_eq!(store.audit_requests(), ["update-request"]);
}

#[tokio::test]
async fn accounts_update_should_not_notify_provider_when_store_commit_fails() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    store.fail_next_commit();
    let services = accounts_service(provider, store).await;

    services
        .accounts()
        .update(
            &context("update-failure"),
            UpdateAccount {
                account_id: "acct_test".to_owned(),
                enabled: false,
                concurrency_limit: None,
                weight: gateway_core::account::AccountWeight::DEFAULT,
                group_ids: Vec::new(),
            },
        )
        .await
        .expect_err("failed account update");

    assert_eq!(
        recorded(&events),
        ["store.load_account", "store.update_account"]
    );
}

#[tokio::test]
async fn accounts_batch_update_should_commit_once_and_notify_each_provider() {
    let events = events();
    let openai = FakeProviderAdmin::new("openai", events.clone());
    let xai = FakeProviderAdmin::new("xai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let mut openai_account = account_record("openai");
    openai_account.id = "acct_openai".to_owned();
    let mut xai_account = account_record("xai");
    xai_account.id = "acct_xai".to_owned();
    store.set_accounts(vec![openai_account, xai_account]);
    let services = super::AdminHarness::new()
        .accounts(store.clone())
        .settings(Arc::new(StaticSettingsStore))
        .provider(openai)
        .provider(xai)
        .probe(Arc::new(SuccessfulAccountProbe))
        .build()
        .await;

    let result = services
        .accounts()
        .batch_update(
            &context("batch-update-request"),
            BatchUpdateAccounts {
                account_ids: vec!["acct_openai".to_owned(), "acct_xai".to_owned()],
                enabled: false,
                concurrency_limit: None,
                weight: gateway_core::account::AccountWeight::DEFAULT,
                group_ids: Vec::new(),
            },
        )
        .await
        .expect("batch update accounts");

    assert_eq!(result.config_revision, revision(2));
    assert_eq!(result.account_ids.len(), 2);
    assert_eq!(store.audit_requests(), ["batch-update-request"]);
    assert_eq!(
        recorded(&events),
        [
            "store.load_account",
            "store.load_account",
            "store.batch_update_accounts",
            "provider.account_unavailable",
            "provider.account_facts_changed",
            "provider.account_unavailable",
            "provider.account_facts_changed",
        ]
    );
}

#[tokio::test]
async fn accounts_list_should_return_complete_directory_semantics() {
    let provider = FakeProviderAdmin::new("openai", events());
    let store = FakeAccountStore::new("openai", events());
    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("complete account directory");

    assert_eq!(page.summary.total, 1);
    assert_eq!(page.summary.normal, 1);
    let account = page.items.first().expect("account item");
    assert_eq!(account.account.provider_kind.as_str(), "openai");
    assert_eq!(
        account.projection.status,
        gateway_admin::model::accounts::AccountStatus::Normal
    );
}

#[tokio::test]
async fn accounts_list_should_degrade_quota_failure_to_empty_window_without_dropping_page() {
    let events = events();
    let openai = FakeProviderAdmin::new("openai", events.clone());
    openai.set_quota(ProviderQuota {
        observed_at: Some(Utc::now()),
        refresh_token_expires_at: None,
        windows: vec![ProviderQuotaWindow {
            key: "primary".to_owned(),
            group: "shortTerm".to_owned(),
            label: "5小时限额".to_owned(),
            limit_id: None,
            limit_name: None,
            role: None,
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds: Some(5 * 60 * 60),
            used_percent: Some(97.0),
            reset_at: Some(Utc::now() + TimeDelta::hours(1)),
            limit_reached: false,
            local_usage: None,
            provider_data: None,
        }],
        limit_reached: false,
        provider_data: None,
    });
    let failing = FakeProviderAdmin::new("xai", events.clone());
    failing.fail_next_quota(ProviderAdminErrorKind::Invalid);
    let store = FakeAccountStore::new("openai", events.clone());
    let mut xai_account = account_record("xai");
    xai_account.id = "acct_bad_xai".to_owned();
    store.set_accounts(vec![account_record("openai"), xai_account]);
    let services = super::AdminHarness::new()
        .accounts(store.clone())
        .settings(Arc::new(StaticSettingsStore))
        .provider(openai)
        .provider(failing)
        .probe(Arc::new(SuccessfulAccountProbe))
        .build()
        .await;

    let page = services
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("one failing quota must not fail the directory page");

    assert_eq!(page.items.len(), 2);
    let by_id = page
        .items
        .iter()
        .map(|item| (item.account.id.as_str(), item))
        .collect::<std::collections::HashMap<_, _>>();
    let healthy = by_id.get("acct_test").expect("healthy account item");
    assert_eq!(healthy.quota.windows.len(), 1);
    assert_eq!(healthy.quota.windows[0].key, "primary");
    let degraded = by_id.get("acct_bad_xai").expect("degraded account item");
    assert!(degraded.quota.windows.is_empty());
    assert!(degraded.quota.observed_at.is_none());
    assert!(degraded.quota.provider_data.is_none());
    assert_eq!(page.summary.total, 2);
}

#[tokio::test]
async fn accounts_list_should_prefer_credential_error_over_quota_exhaustion() {
    let provider = FakeProviderAdmin::new("openai", events());
    let mut account = account_record("openai");
    account.quota = QuotaState::exhausted(
        QuotaEvidence::ProviderDenied,
        std::time::SystemTime::now(),
        None,
    );
    account.access_token_expires_at = Some(Utc::now() - TimeDelta::minutes(5));
    let store = FakeAccountStore::with_account(account, events());

    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("usage-limited account directory");

    assert_eq!(
        page.items.first().expect("account item").projection.status,
        gateway_admin::model::accounts::AccountStatus::Error,
    );
}

#[tokio::test]
async fn accounts_list_should_map_unknown_credential_to_error_not_normal() {
    // Unknown 不可调度，Admin 不得显示为 normal。
    let provider = FakeProviderAdmin::new("openai", events());
    let mut account = account_record("openai");
    account.credential_state = CredentialState::Unknown;
    let store = FakeAccountStore::with_account(account, events());

    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("unknown account directory");

    assert_eq!(
        page.items.first().expect("account item").projection.status,
        gateway_admin::model::accounts::AccountStatus::Error,
    );
}

#[tokio::test]
async fn accounts_list_should_not_derive_rate_limited_from_provider_quota_view() {
    let provider = FakeProviderAdmin::new("openai", events());
    provider.set_quota(ProviderQuota {
        observed_at: Some(Utc::now()),
        refresh_token_expires_at: None,
        windows: vec![ProviderQuotaWindow {
            key: "primary".to_owned(),
            group: "shortTerm".to_owned(),
            label: "5小时限额".to_owned(),
            limit_id: None,
            limit_name: None,
            role: None,
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds: Some(5 * 60 * 60),
            used_percent: Some(100.0),
            reset_at: Some(Utc::now() + TimeDelta::hours(1)),
            limit_reached: true,
            local_usage: None,
            provider_data: None,
        }],
        limit_reached: true,
        provider_data: None,
    });
    let store = FakeAccountStore::new("openai", events());

    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("account directory with display quota cooldown");

    assert_eq!(
        page.items.first().expect("account item").projection.status,
        gateway_admin::model::accounts::AccountStatus::Normal,
    );
}

#[tokio::test]
async fn accounts_list_should_not_derive_exhaustion_from_provider_quota_view() {
    let provider = FakeProviderAdmin::new("openai", events());
    provider.set_quota(ProviderQuota {
        observed_at: Some(Utc::now()),
        refresh_token_expires_at: None,
        windows: vec![ProviderQuotaWindow {
            key: "primary".to_owned(),
            group: "shortTerm".to_owned(),
            label: "5小时限额".to_owned(),
            limit_id: None,
            limit_name: None,
            role: None,
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds: Some(5 * 60 * 60),
            used_percent: Some(100.0),
            reset_at: Some(Utc::now() + TimeDelta::hours(1)),
            limit_reached: true,
            local_usage: None,
            provider_data: None,
        }],
        limit_reached: true,
        provider_data: None,
    });
    let store = FakeAccountStore::new("openai", events());

    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("account directory with display quota limit");

    assert_eq!(
        page.items.first().expect("account item").projection.status,
        gateway_admin::model::accounts::AccountStatus::Normal,
    );
}

#[tokio::test]
async fn accounts_list_should_attach_local_usage_to_quota_windows() {
    let provider = FakeProviderAdmin::new("openai", events());
    let reset_at = Utc::now() + TimeDelta::hours(1);
    provider.set_quota(ProviderQuota {
        observed_at: Some(Utc::now()),
        refresh_token_expires_at: None,
        windows: vec![ProviderQuotaWindow {
            key: "primary".to_owned(),
            group: "shortTerm".to_owned(),
            label: "5小时限额".to_owned(),
            limit_id: None,
            limit_name: None,
            role: None,
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds: Some(5 * 60 * 60),
            used_percent: Some(97.0),
            reset_at: Some(reset_at),
            limit_reached: false,
            local_usage: None,
            provider_data: None,
        }],
        limit_reached: false,
        provider_data: None,
    });
    let store = FakeAccountStore::new("openai", events());
    store.set_quota_window_usage(vec![AccountUsageWindowResult {
        account_id: "acct_test".to_owned(),
        key: "primary".to_owned(),
        usage: quota_local_usage("acct_test", 4_330_000),
    }]);

    let page = accounts_service(provider, store.clone())
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("quota window usage");

    let item = &page.items[0];
    let usage = item.quota.windows[0]
        .local_usage
        .as_ref()
        .expect("quota window local usage");
    assert_eq!(usage.total_tokens, Some(4_330_000));
    assert_eq!(
        item.usage.as_ref().and_then(|usage| usage.total_tokens),
        Some(4_330_000),
    );

    let queries = store.quota_window_queries();
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].account_id, "acct_test");
    assert_eq!(queries[0].key, "primary");
    assert_eq!(queries[0].range.start, reset_at - TimeDelta::hours(5));
    assert_eq!(queries[0].range.end, reset_at);
}

#[tokio::test]
async fn accounts_list_should_not_attach_account_usage_to_model_specific_quota_windows() {
    let provider = FakeProviderAdmin::new("openai", events());
    let reset_at = Utc::now() + TimeDelta::days(7);
    provider.set_quota(ProviderQuota {
        observed_at: Some(Utc::now()),
        refresh_token_expires_at: None,
        windows: vec![
            ProviderQuotaWindow {
                key: "core-primary".to_owned(),
                group: "weekly".to_owned(),
                label: "周额度".to_owned(),
                limit_id: Some("codex".to_owned()),
                limit_name: None,
                role: None,
                local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
                window_seconds: Some(7 * 24 * 60 * 60),
                used_percent: Some(1.0),
                reset_at: Some(reset_at),
                limit_reached: false,
                local_usage: None,
                provider_data: None,
            },
            ProviderQuotaWindow {
                key: "codex-bengalfox-primary".to_owned(),
                group: "weekly".to_owned(),
                label: "周额度".to_owned(),
                limit_id: Some("codex_bengalfox".to_owned()),
                limit_name: Some("GPT-5.3-Codex-Spark".to_owned()),
                role: None,
                local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
                window_seconds: Some(7 * 24 * 60 * 60),
                used_percent: Some(0.0),
                reset_at: Some(reset_at),
                limit_reached: false,
                local_usage: None,
                provider_data: None,
            },
        ],
        limit_reached: false,
        provider_data: None,
    });
    let store = FakeAccountStore::new("openai", events());
    store.set_quota_window_usage(vec![
        AccountUsageWindowResult {
            account_id: "acct_test".to_owned(),
            key: "core-primary".to_owned(),
            usage: quota_local_usage("acct_test", 12_818_806),
        },
        AccountUsageWindowResult {
            account_id: "acct_test".to_owned(),
            key: "codex-bengalfox-primary".to_owned(),
            usage: quota_local_usage("acct_test", 127_926),
        },
    ]);

    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            group_filter: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("quota window usage");

    let local_tokens = page.items[0]
        .quota
        .windows
        .iter()
        .map(|window| {
            (
                window.key.as_str(),
                window
                    .local_usage
                    .as_ref()
                    .and_then(|usage| usage.total_tokens),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        local_tokens,
        [
            ("core-primary", Some(12_818_806)),
            ("codex-bengalfox-primary", None),
        ],
    );
    assert_eq!(
        page.items[0]
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens),
        Some(12_818_806),
    );
}

#[tokio::test]
async fn accounts_refresh_provider_failure_should_not_call_store_commit() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.fail_next(ProviderAdminErrorKind::Unavailable);
    let store = FakeAccountStore::new("openai", events.clone());
    let services = accounts_service(provider, store).await;

    services
        .accounts()
        .refresh(
            &context("refresh-provider-error"),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect_err("Provider preparation must fail");

    assert_eq!(
        recorded(&events),
        ["store.load_account", "provider.prepare_refresh"]
    );
}

#[tokio::test]
async fn reset_credit_oauth_refresh_should_reuse_the_exact_consume_command() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.fail_next(ProviderAdminErrorKind::CredentialRefreshRequired);
    let store = FakeAccountStore::new("openai", events.clone());
    let services = accounts_service(provider.clone(), store).await;
    let redeem_request_id =
        uuid::Uuid::parse_str("8fbf302d-11df-4bd5-82e4-08e4b3df7874").expect("UUID v4");
    let command = ConsumeProviderResetCredit {
        account_id: ProviderAccountId::new("acct_test").expect("account ID"),
        credit_id: Some("credit_1".to_owned()),
        redeem_request_id,
    };

    let result = services
        .accounts()
        .consume_reset_credit(&context("reset-credit-refresh"), command.clone())
        .await
        .expect("consume after OAuth refresh");

    assert_eq!(result.code, "reset");
    assert_eq!(provider.reset_credit_commands(), [command.clone(), command]);
    assert!(recorded(&events).contains(&"provider.prepare_refresh"));
    assert!(recorded(&events).contains(&"store.commit_refresh"));
}

#[tokio::test]
async fn reset_credit_unknown_result_should_keep_a_stable_admin_kind() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.fail_next(ProviderAdminErrorKind::Ambiguous);
    let store = FakeAccountStore::new("openai", events);
    let services = accounts_service(provider, store).await;
    let command = ConsumeProviderResetCredit {
        account_id: ProviderAccountId::new("acct_test").expect("account ID"),
        credit_id: Some("credit_1".to_owned()),
        redeem_request_id: uuid::Uuid::parse_str("244e790c-42a3-4ec9-a45d-a32b218bc8ac")
            .expect("UUID v4"),
    };

    let error = services
        .accounts()
        .consume_reset_credit(&context("reset-credit-ambiguous"), command)
        .await
        .expect_err("ambiguous consume result must remain classified");

    assert_eq!(
        error.kind(),
        gateway_admin::model::AdminErrorKind::UpstreamResultUnknown
    );
    assert_eq!(
        error.to_string(),
        "上游执行结果未知，请刷新状态后再决定是否重试"
    );
}

#[tokio::test]
async fn accounts_refresh_should_not_expose_the_provider_failure_message() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let upstream_message = "Your refresh token has already been used.";
    provider.fail_next_with_message(ProviderAdminErrorKind::Conflict, upstream_message);
    let store = FakeAccountStore::new("openai", events.clone());
    let services = accounts_service(provider, store).await;

    let error = services
        .accounts()
        .refresh(
            &context("refresh-provider-message"),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect_err("Provider preparation must fail");

    assert_eq!(error.kind(), gateway_admin::model::AdminErrorKind::Conflict);
    assert_eq!(error.to_string(), "Provider 资源状态冲突，请刷新后重试");
    assert!(!error.to_string().contains(upstream_message));
    assert_eq!(
        recorded(&events),
        ["store.load_account", "provider.prepare_refresh"]
    );
}

#[tokio::test]
async fn accounts_refresh_store_failure_should_drop_guard_after_commit_attempt() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    store.fail_next_commit();
    let services = accounts_service(provider, store).await;

    services
        .accounts()
        .refresh(
            &context("refresh-store-error"),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect_err("Store commit must fail");

    assert_eq!(
        recorded(&events),
        [
            "store.load_account",
            "provider.prepare_refresh",
            "store.commit_refresh",
            "guard.drop",
        ]
    );
}

pub(super) fn events() -> EventLog {
    Arc::new(Mutex::new(Vec::new()))
}

pub(super) fn recorded(events: &EventLog) -> Vec<&'static str> {
    events.lock().expect("recorded events").clone()
}

pub(super) fn context(request_id: &str) -> MutationContext {
    MutationContext {
        actor: gateway_admin::model::MutationActor::AdminSession {
            admin_user_id: "admin-test".to_owned(),
        },
        request_id: request_id.to_owned(),
    }
}

pub(super) fn document() -> ProviderDocument {
    ProviderDocument::new(OpaqueProviderData::new(Default::default()))
}

fn empty_quota() -> ProviderQuota {
    ProviderQuota {
        observed_at: None,
        refresh_token_expires_at: None,
        windows: Vec::new(),
        limit_reached: false,
        provider_data: None,
    }
}

fn quota_local_usage(account_id: &str, total_tokens: u64) -> AccountUsage {
    AccountUsage {
        account_id: account_id.to_owned(),
        request_count: 1,
        success_count: 1,
        input_tokens: Some(total_tokens),
        output_tokens: Some(0),
        cached_tokens: Some(0),
        cache_write_tokens: Some(0),
        reasoning_tokens: Some(0),
        image_input_tokens: Some(0),
        image_output_tokens: Some(0),
        image_request_count: 0,
        image_request_failed_count: 0,
        total_tokens: Some(total_tokens),
        cost_coverage: Default::default(),
        costs: Vec::new(),
        last_used_at: Some(Utc::now()),
        request_buckets: Vec::new(),
        models: Vec::new(),
    }
}

pub(super) fn account_record(kind: &str) -> AccountRecord {
    let now = Utc::now();
    AccountRecord {
        id: "acct_test".to_owned(),
        provider_kind: ProviderKind::new(kind).expect("provider kind"),
        groups: Vec::new(),
        name: "test account".to_owned(),
        email: Some("test@example.invalid".to_owned()),
        upstream_user_id: Some("upstream-user".to_owned()),
        upstream_account_id: None,
        plan_type: Some("test".to_owned()),
        authentication_kind: "oauth".to_owned(),
        credential_revision: revision(1),
        has_refresh_token: true,
        access_token_expires_at: Some(now + TimeDelta::hours(1)),
        next_refresh_at: Some(now + TimeDelta::minutes(30)),
        enabled: true,
        concurrency_limit: None,
        weight: gateway_core::account::AccountWeight::DEFAULT,
        credential_state: CredentialState::Ready,
        credential_observed_at: now,
        quota: QuotaState::allowed(now.into()),
        last_error_reason: None,
        last_error_message: None,
        created_at: now,
        updated_at: now,
    }
}

pub(super) fn revision(value: u64) -> Revision {
    Revision::new(value).expect("positive revision")
}

fn prepared_create(provider_kind: ProviderKind, name: &str) -> PreparedCredentialCreate {
    prepared_create_with_id(provider_kind, "acct_prepared", name)
}

fn prepared_create_with_id(
    provider_kind: ProviderKind,
    account_id: &str,
    name: &str,
) -> PreparedCredentialCreate {
    let now = Utc::now();
    PreparedCredentialCreate {
        account_id: ProviderAccountId::new(account_id).expect("prepared account ID"),
        provider_kind,
        name: name.to_owned(),
        email: Some("prepared@example.invalid".to_owned()),
        upstream_user_id: Some("prepared-user".to_owned()),
        upstream_account_id: None,
        plan_type: Some("test".to_owned()),
        authentication_kind: "oauth".to_owned(),
        provider_material: document(),
        has_refresh_token: true,
        access_token_expires_at: Some(now + TimeDelta::hours(1)),
        next_refresh_at: Some(now + TimeDelta::minutes(30)),
        enabled: true,
        credential_state: CredentialState::Ready,
        credential_observed_at: now,
    }
}

fn rotation_result(command: CredentialRotationCommit) -> CredentialMutationResult {
    CredentialMutationResult {
        config_revision: revision(2),
        account_id: command.prepared.account_id,
        credential_revision: Some(revision(
            command.prepared.expected_credential_revision.get() + 1,
        )),
    }
}

async fn accounts_service(
    provider: Arc<FakeProviderAdmin>,
    store: Arc<FakeAccountStore>,
) -> AdminServices {
    accounts_service_with_probe(provider, store, Arc::new(SuccessfulAccountProbe)).await
}

async fn accounts_service_with_probe(
    provider: Arc<FakeProviderAdmin>,
    store: Arc<FakeAccountStore>,
    probe: Arc<dyn AccountProbe>,
) -> AdminServices {
    super::AdminHarness::new()
        .accounts(store)
        .settings(Arc::new(StaticSettingsStore))
        .provider(provider)
        .probe(probe)
        .build()
        .await
}

struct SuccessfulAccountProbe;

impl AccountProbe for SuccessfulAccountProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async {
            Ok(AccountProbeResult {
                text: vec!["OK".to_owned()],
            })
        })
    }
}

struct FailingAccountProbe;

impl AccountProbe for FailingAccountProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async {
            let detail = ClientVisibleUpstreamError::new(
                "included usage exhausted",
                Some("usage_exhausted".to_owned()),
                Some("invalid_request_error".to_owned()),
            );
            Err(AccountProbeError::new(
                GatewayError::new(
                    GatewayErrorKind::RateLimited,
                    "upstream capacity is temporarily unavailable",
                )
                .with_client_visible_upstream_error(detail),
                AccountProbeErrorSource::Upstream,
                Some(UpstreamSendState::NotSent),
                None,
            ))
        })
    }
}

struct FreeModelQuotaProbe {
    store: Arc<FakeAccountStore>,
}

impl AccountProbe for FreeModelQuotaProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async move {
            self.store.set_account_after_probe(account_record("xai"));
            Err(AccountProbeError::new(
                GatewayError::new(
                    GatewayErrorKind::RateLimited,
                    "xAI free model quota is exhausted",
                ),
                AccountProbeErrorSource::Provider,
                Some(UpstreamSendState::NotSent),
                None,
            ))
        })
    }
}

fn store_unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "test account",
        "unavailable",
    )
}

fn unsupported() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unsupported)
}
