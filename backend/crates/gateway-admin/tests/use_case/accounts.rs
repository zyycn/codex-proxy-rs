use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use futures::future::BoxFuture;
use gateway_core::{
    engine::{
        credential::{OpaqueProviderData, ProviderAccountId},
        probe::{AccountProbe, AccountProbeRequest, AccountProbeResult},
    },
    error::GatewayError,
    routing::{ProviderInstanceId, ProviderKind},
};

use gateway_admin::{
    AdminServices,
    model::{
        MutationContext, Revision,
        accounts::{
            AccountAvailability, AccountListQuery, AccountPage, AccountRecord, AccountSummary,
            AccountUsage, DeleteAccount, SetAccountEnabled,
        },
        catalog::{
            CreateProviderInstance, DeleteProviderInstance, ProviderInstance,
            ProviderInstanceCatalog, ProviderInstanceDetail, ProviderInstanceMutation,
            SetProviderInstanceEnabled, UpdateProviderInstance,
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
            AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStoreResult, CatalogStore,
            SettingsStore,
        },
    },
};

pub(super) type EventLog = Arc<Mutex<Vec<&'static str>>>;

pub(super) struct FakeProviderAdmin {
    kind: ProviderKind,
    events: EventLog,
    failure: Mutex<Option<ProviderAdminErrorKind>>,
    pending: Mutex<Option<PendingAuthorizationMutation>>,
    export_inputs: Mutex<Vec<ProviderExportCredentialInput>>,
}

impl FakeProviderAdmin {
    pub(super) fn new(kind: &str, events: EventLog) -> Arc<Self> {
        Arc::new(Self {
            kind: ProviderKind::new(kind).expect("provider kind"),
            events,
            failure: Mutex::new(None),
            pending: Mutex::new(None),
            export_inputs: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn fail_next(&self, kind: ProviderAdminErrorKind) {
        *self.failure.lock().expect("provider failure") = Some(kind);
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
                provider_instance_id: account.provider_instance_id.clone(),
                provider_kind: account.provider_kind.clone(),
                expected_credential_revision: account.credential_revision,
                name: account.name.clone(),
                email: account.email.clone(),
                plan_type: account.plan_type.clone(),
                provider_material: document(),
                has_refresh_token: account.has_refresh_token,
                access_token_expires_at: account.access_token_expires_at + TimeDelta::hours(1),
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
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        self.record("provider.prepare_import");
        self.require_available()?;
        Ok(PreparedCredentialImport {
            provider_kind: self.kind.clone(),
            provider_instance_id: command.provider_instance_id.clone(),
            credentials: vec![prepared_create(
                self.kind.clone(),
                command.provider_instance_id,
                "prepared-import",
            )],
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
            AuthorizationMutationTarget::Create {
                provider_instance_id,
                name,
            } => PreparedAuthorizationCredential::Create(prepared_create(
                self.kind.clone(),
                provider_instance_id.clone(),
                name,
            )),
            AuthorizationMutationTarget::Reauthorize {
                provider_instance_id,
                account_id,
                expected_credential_revision,
            } => {
                let mut account = account_record(self.kind.as_str());
                account.id = account_id.as_str().to_owned();
                account.provider_instance_id = provider_instance_id.clone();
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
        _: &ProviderAccountId,
        _: bool,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        Ok(ProviderQuota {
            observed_at: None,
            refresh_token_expires_at: None,
            windows: Vec::new(),
            provider_data: None,
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
    account: AccountRecord,
    fail_commit: Mutex<bool>,
    audit_requests: Mutex<Vec<String>>,
    authorization_revision: Mutex<Option<Revision>>,
}

impl FakeAccountStore {
    pub(super) fn new(kind: &str, events: EventLog) -> Arc<Self> {
        Arc::new(Self {
            events,
            account: account_record(kind),
            fail_commit: Mutex::new(false),
            audit_requests: Mutex::new(Vec::new()),
            authorization_revision: Mutex::new(None),
        })
    }

    pub(super) fn fail_next_commit(&self) {
        *self.fail_commit.lock().expect("store failure") = true;
    }

    pub(super) fn audit_requests(&self) -> Vec<String> {
        self.audit_requests.lock().expect("audit requests").clone()
    }

    pub(super) fn authorization_revision(&self) -> Option<Revision> {
        *self
            .authorization_revision
            .lock()
            .expect("authorization revision")
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
                attention: 0,
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
        *self
            .authorization_revision
            .lock()
            .expect("authorization revision") = Some(command.pending.expected_config_revision());
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

    async fn delete_account(
        &self,
        _: DeleteAccount,
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
            provider_model_mappings: Default::default(),
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
        _: Revision,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(store_unavailable())
    }

    async fn delete_admin_api_key(
        &self,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(store_unavailable())
    }
}

struct StaticCatalogStore {
    instance: ProviderInstance,
}

#[async_trait]
impl CatalogStore for StaticCatalogStore {
    async fn list_provider_instances(&self, _: bool) -> AdminStoreResult<ProviderInstanceCatalog> {
        Ok(ProviderInstanceCatalog {
            config_revision: revision(1),
            items: vec![self.instance.clone()],
        })
    }

    async fn load_provider_instance(
        &self,
        id: &ProviderInstanceId,
    ) -> AdminStoreResult<Option<ProviderInstanceDetail>> {
        Ok((id == &self.instance.id).then(|| ProviderInstanceDetail {
            config_revision: revision(1),
            item: self.instance.clone(),
        }))
    }

    async fn create_provider_instance(
        &self,
        _: CreateProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(store_unavailable())
    }

    async fn update_provider_instance(
        &self,
        _: UpdateProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(store_unavailable())
    }

    async fn set_provider_instance_enabled(
        &self,
        _: SetProviderInstanceEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(store_unavailable())
    }

    async fn delete_provider_instance(
        &self,
        _: DeleteProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
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
            revision(1),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect("refresh credential");
    assert_eq!(result.config_revision, revision(2));
    assert_eq!(result.account.provider_instance_name, "Test Provider");
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
    assert_eq!(account.provider_instance_name, "Test Provider");
    assert_eq!(
        account.status,
        gateway_admin::model::accounts::AccountStatus::Active
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
            revision(1),
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
async fn accounts_refresh_store_failure_should_drop_guard_after_cas_attempt() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    store.fail_next_commit();
    let services = accounts_service(provider, store).await;

    services
        .accounts()
        .refresh(
            &context("refresh-store-error"),
            revision(1),
            ProviderAccountId::new("acct_test").expect("account ID"),
        )
        .await
        .expect_err("Store CAS must fail");

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

pub(super) fn account_record(kind: &str) -> AccountRecord {
    let now = Utc::now();
    AccountRecord {
        id: "acct_test".to_owned(),
        provider_instance_id: ProviderInstanceId::new(format!("inst_{kind}")).expect("instance ID"),
        provider_kind: ProviderKind::new(kind).expect("provider kind"),
        name: "test account".to_owned(),
        email: Some("test@example.invalid".to_owned()),
        upstream_user_id: "upstream-user".to_owned(),
        upstream_account_id: None,
        plan_type: Some("test".to_owned()),
        credential_revision: revision(1),
        has_refresh_token: true,
        access_token_expires_at: now + TimeDelta::hours(1),
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

fn prepared_create(
    provider_kind: ProviderKind,
    provider_instance_id: ProviderInstanceId,
    name: &str,
) -> PreparedCredentialCreate {
    let now = Utc::now();
    PreparedCredentialCreate {
        account_id: ProviderAccountId::new("acct_prepared").expect("prepared account ID"),
        provider_instance_id,
        provider_kind,
        name: name.to_owned(),
        email: Some("prepared@example.invalid".to_owned()),
        upstream_user_id: "prepared-user".to_owned(),
        upstream_account_id: None,
        plan_type: Some("test".to_owned()),
        provider_material: document(),
        has_refresh_token: true,
        access_token_expires_at: now + TimeDelta::hours(1),
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
        config_revision: revision(command.expected_config_revision.get() + 1),
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
    let now = Utc::now();
    let instance = ProviderInstance {
        id: store.account.provider_instance_id.clone(),
        provider_kind: store.account.provider_kind.clone(),
        name: "Test Provider".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    super::AdminHarness::new()
        .accounts(store)
        .catalog(Arc::new(StaticCatalogStore { instance }))
        .settings(Arc::new(StaticSettingsStore))
        .provider(provider)
        .probe(Arc::new(SuccessfulAccountProbe))
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
