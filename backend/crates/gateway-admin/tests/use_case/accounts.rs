use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::{
    engine::{
        credential::{OpaqueProviderData, ProviderAccountId},
        probe::{AccountProbe, AccountProbeRequest, AccountProbeResult},
    },
    error::{ClientVisibleUpstreamError, GatewayError, GatewayErrorKind},
    operation::{GenerateRequest, Operation, ProtocolPayload},
    routing::ProviderKind,
};

use gateway_admin::{
    AdminServices,
    model::{
        MutationContext, Revision,
        accounts::{
            AccountAvailability, AccountConnectionTestEvent, AccountListQuery, AccountPage,
            AccountRecord, AccountSummary, AccountUsage, AccountUsageWindowQuery,
            AccountUsageWindowResult, DeleteAccounts, SetAccountEnabled,
        },
        observability::TimeRange,
        provider_credentials::{
            AuthorizationCommit, AuthorizationCredentialCommit, AuthorizationMutationTarget,
            AuthorizationStarted, CompleteAuthorization, CredentialCommitGuard, CredentialDetails,
            CredentialImportCommit, CredentialImportResult, CredentialListQuery,
            CredentialMutationResult, CredentialPage, CredentialRotationCommit,
            PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
            PrepareCredentialRotation, PreparedAuthorizationCommit,
            PreparedAuthorizationCredential, PreparedCredentialCreate, PreparedCredentialImport,
            PreparedCredentialRotation, PreparedCredentialRotationFacts, ProviderDocument,
            ProviderExport, ProviderExportCredentialInput, ProviderModels, ProviderQuota,
            ProviderQuotaRequest, ProviderQuotaWindow,
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
    failure: Mutex<Option<ProviderAdminErrorKind>>,
    quota_failure: Mutex<Option<ProviderAdminErrorKind>>,
    pending: Mutex<Option<PendingAuthorizationMutation>>,
    export_inputs: Mutex<Vec<ProviderExportCredentialInput>>,
    import_account_ids: Mutex<Vec<String>>,
    quota_requests: Mutex<Vec<ProviderQuotaRequest>>,
    quota: Mutex<ProviderQuota>,
}

impl FakeProviderAdmin {
    pub(super) fn new(kind: &str, events: EventLog) -> Arc<Self> {
        Arc::new(Self {
            kind: ProviderKind::new(kind).expect("provider kind"),
            events,
            failure: Mutex::new(None),
            quota_failure: Mutex::new(None),
            pending: Mutex::new(None),
            export_inputs: Mutex::new(Vec::new()),
            import_account_ids: Mutex::new(vec!["acct_prepared".to_owned()]),
            quota_requests: Mutex::new(Vec::new()),
            quota: Mutex::new(empty_quota()),
        })
    }

    pub(super) fn fail_next(&self, kind: ProviderAdminErrorKind) {
        *self.failure.lock().expect("provider failure") = Some(kind);
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

    pub(super) fn set_quota(&self, quota: ProviderQuota) {
        *self.quota.lock().expect("provider quota") = quota;
    }

    pub(super) fn pending(&self) -> Option<PendingAuthorizationMutation> {
        self.pending.lock().expect("pending authorization").clone()
    }

    fn export_inputs(&self) -> Vec<ProviderExportCredentialInput> {
        self.export_inputs.lock().expect("export inputs").clone()
    }

    fn record(&self, event: &'static str) {
        self.events.lock().expect("provider events").push(event);
    }

    fn require_available(&self) -> Result<(), ProviderAdminError> {
        match self.failure.lock().expect("provider failure").take() {
            Some(kind) => Err(ProviderAdminError::new(kind)),
            None => Ok(()),
        }
    }

    fn prepared_rotation(&self, account: &AccountRecord) -> PreparedCredentialRotation {
        PreparedCredentialRotation::new(
            PreparedCredentialRotationFacts {
                account_id: ProviderAccountId::new(account.id.clone()).expect("account ID"),
                provider_kind: account.provider_kind.clone(),
                expected_credential_revision: account.credential_revision,
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
        let credential = match pending.target() {
            AuthorizationMutationTarget::Create { name } => {
                PreparedAuthorizationCredential::Create(prepared_create(self.kind.clone(), name))
            }
            AuthorizationMutationTarget::Reauthorize {
                account_id,
                expected_credential_revision,
            } => {
                let mut account = account_record(self.kind.as_str());
                account.id = account_id.as_str().to_owned();
                account.credential_revision = *expected_credential_revision;
                PreparedAuthorizationCredential::Reauthorize(self.prepared_rotation(&account))
            }
        };
        Ok(PreparedAuthorizationCommit {
            pending,
            credential,
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
    account: AccountRecord,
    fail_commit: Mutex<bool>,
    audit_requests: Mutex<Vec<String>>,
    quota_window_usage: Mutex<Vec<AccountUsageWindowResult>>,
}

impl FakeAccountStore {
    pub(super) fn new(kind: &str, events: EventLog) -> Arc<Self> {
        Self::with_account(account_record(kind), events)
    }

    fn with_account(account: AccountRecord, events: EventLog) -> Arc<Self> {
        Arc::new(Self {
            events,
            account,
            fail_commit: Mutex::new(false),
            audit_requests: Mutex::new(Vec::new()),
            quota_window_usage: Mutex::new(Vec::new()),
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
}

#[async_trait]
impl AccountStore for FakeAccountStore {
    async fn list_accounts(&self, _: AccountListQuery) -> AdminStoreResult<AccountPage> {
        Ok(AccountPage {
            config_revision: revision(1),
            items: vec![self.account.clone()],
            total: 1,
            summary: AccountSummary {
                total: 1,
                active: 1,
                quota_exhausted: 0,
                unavailable: 0,
            },
        })
    }

    async fn load_account(&self, account_id: &str) -> AdminStoreResult<Option<AccountRecord>> {
        self.record("store.load_account");
        Ok((account_id == self.account.id).then(|| self.account.clone()))
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
        _: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
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
        Ok(CredentialPage {
            config_revision: revision(1),
            items: (provider_kind == &self.account.provider_kind)
                .then(|| self.account.clone())
                .into_iter()
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
        Ok(
            (provider_kind == &self.account.provider_kind
                && account_id.as_str() == self.account.id)
                .then(|| CredentialDetails {
                    config_revision: revision(1),
                    credential: self.account.clone(),
                }),
        )
    }

    async fn load_credentials_for_export(
        &self,
        provider_kind: &ProviderKind,
        account_ids: &[ProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>> {
        self.record("store.load_credentials_for_export");
        if provider_kind != &self.account.provider_kind
            || account_ids
                .iter()
                .any(|account_id| account_id.as_str() != self.account.id)
        {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::NotFound,
                "test account",
                "credential not found",
            ));
        }
        Ok(vec![ProviderExportCredentialInput {
            account: self.account.clone(),
            provider_material: document(),
        }])
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

    async fn set_account_enabled(
        &self,
        _: SetAccountEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        self.record("store.set_enabled");
        self.record_context(context);
        self.require_commit()?;
        Ok(revision(2))
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
    account.availability = AccountAvailability::QuotaExhausted;
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
            message,
            provider_error_code: Some(code),
            provider_error_type: Some(error_type),
            account_status: gateway_admin::model::accounts::AccountStatus::QuotaExhausted,
        }) if message == "included usage exhausted"
            && code == "usage_exhausted"
            && error_type == "invalid_request_error"
    ));
}

#[tokio::test]
async fn accounts_export_should_pass_store_loaded_timestamps_and_material_to_provider() {
    let provider = FakeProviderAdmin::new("openai", events());
    let store = FakeAccountStore::new("openai", events());
    let expected = store.account.clone();
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
        result.account.status,
        gateway_admin::model::accounts::AccountStatus::Active
    );

    assert_eq!(
        recorded(&events),
        [
            "store.load_account",
            "provider.prepare_refresh",
            "store.commit_refresh",
            "guard.finish",
            "store.load_account",
        ]
    );
    assert_eq!(store.audit_requests(), ["refresh-request"]);
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
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("complete account directory");

    assert_eq!(page.summary.total, 1);
    assert_eq!(page.summary.active, 1);
    let account = page.items.first().expect("account item");
    assert_eq!(account.account.provider_kind.as_str(), "openai");
    assert_eq!(
        account.status,
        gateway_admin::model::accounts::AccountStatus::Active
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
            source: None,
            window_seconds: Some(5 * 60 * 60),
            used_percent: Some(97.0),
            reset_at: Some(reset_at),
            local_usage: None,
            provider_data: None,
        }],
        provider_data: None,
    });
    let store = FakeAccountStore::new("openai", events());
    store.set_quota_window_usage(vec![AccountUsageWindowResult {
        account_id: "acct_test".to_owned(),
        key: "primary".to_owned(),
        usage: quota_local_usage("acct_test", 4_330_000),
    }]);

    let page = accounts_service(provider, store)
        .await
        .accounts()
        .list(AccountListQuery {
            page: 1,
            page_size: gateway_admin::model::PageSize::new(20).expect("page size"),
            provider_kind: None,
            search: None,
            status: None,
            sort: None,
        })
        .await
        .expect("quota window usage");

    let usage = page.items[0].quota.windows[0]
        .local_usage
        .as_ref()
        .expect("quota window local usage");
    assert_eq!(usage.total_tokens, Some(4_330_000));
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
        name: "test account".to_owned(),
        email: Some("test@example.invalid".to_owned()),
        upstream_user_id: "upstream-user".to_owned(),
        upstream_account_id: None,
        plan_type: Some("test".to_owned()),
        authentication_kind: "oauth".to_owned(),
        credential_revision: revision(1),
        has_refresh_token: true,
        access_token_expires_at: Some(now + TimeDelta::hours(1)),
        next_refresh_at: Some(now + TimeDelta::minutes(30)),
        enabled: true,
        availability: AccountAvailability::Ready,
        availability_reason: None,
        cooldown_until: None,
        availability_observed_at: now,
        quota_observed_at: None,
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
        upstream_user_id: "prepared-user".to_owned(),
        upstream_account_id: None,
        plan_type: Some("test".to_owned()),
        authentication_kind: "oauth".to_owned(),
        provider_material: document(),
        has_refresh_token: true,
        access_token_expires_at: Some(now + TimeDelta::hours(1)),
        next_refresh_at: Some(now + TimeDelta::minutes(30)),
        enabled: true,
        availability: AccountAvailability::Ready,
        availability_reason: None,
        cooldown_until: None,
        availability_observed_at: now,
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
    ) -> BoxFuture<'_, Result<AccountProbeResult, GatewayError>> {
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
    ) -> BoxFuture<'_, Result<AccountProbeResult, GatewayError>> {
        Box::pin(async {
            let detail = ClientVisibleUpstreamError::new(
                "included usage exhausted",
                Some("usage_exhausted".to_owned()),
                Some("invalid_request_error".to_owned()),
            )
            .expect("safe provider error");
            Err(GatewayError::new(
                GatewayErrorKind::RateLimited,
                "upstream capacity is temporarily unavailable",
            )
            .with_client_visible_upstream_error(detail))
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
