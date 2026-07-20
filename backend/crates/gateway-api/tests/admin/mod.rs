use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use futures::future::BoxFuture;
use gateway_admin::{
    AdminConfig, AdminServices, InitialAdminPassword,
    model::{
        MutationContext, Revision,
        accounts::{
            AccountListQuery, AccountPage, AccountRecord, AccountUsage, DeleteAccount,
            SetAccountEnabled,
        },
        auth::{AdminAuditEvent, AdminSession},
        catalog::{
            CreateProviderInstance, DeleteProviderInstance, ProviderInstanceCatalog,
            ProviderInstanceDetail, ProviderInstanceMutation, SetProviderInstanceEnabled,
            UpdateProviderInstance,
        },
        client_keys::{
            ClientKeyListQuery, ClientKeyPage, ClientKeyRecord, ClientKeySecret, DeleteClientKey,
            NewClientKey, SetClientKeyEnabled, UpdateClientKey,
        },
        observability::{
            DashboardObservation, DiagnosticDimension, DiagnosticObservation, OpsErrorPage,
            OpsErrorQuery, RequestMetricPoint, TimeRange, UsageDetail, UsageFilter, UsageOverview,
            UsagePage, UsageQuery,
        },
        provider_credentials::{
            AuthorizationCommit, AuthorizationStarted, CompleteAuthorization, CredentialDetails,
            CredentialImportCommit, CredentialImportResult, CredentialListQuery,
            CredentialMutationResult, CredentialPage, CredentialRotationCommit,
            PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
            PrepareCredentialRotation, PreparedAuthorizationCommit, PreparedCredentialImport,
            PreparedCredentialRotation, ProviderExport, ProviderExportCredentialInput,
            ProviderModels, ProviderQuota,
        },
        settings::{
            AdminApiKey, AdminApiKeyMutation, ProviderModelMappings, ReplaceRuntimeSettings,
            RotationStrategy, RuntimeSettings,
        },
        system::{SystemOperationAccepted, SystemUpdateDetail, SystemUpdateStatus, SystemVersion},
    },
    ports::{
        provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind},
        store::{
            AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStorePorts, AdminStoreResult,
            AuthStore, CatalogStore, ClientKeyStore, ObservabilityStore, SettingsStore,
        },
        system::{
            SystemOperationError, SystemOperationErrorKind, SystemOperations,
            SystemUpdateEventStream,
        },
    },
};
use gateway_api::admin::AdminSessionState;
use gateway_core::{
    engine::{
        credential::ProviderAccountId,
        probe::{AccountProbe, AccountProbeRequest, AccountProbeResult},
    },
    error::GatewayError,
    policy::{ClientApiKeyId, RateLimits},
    routing::{
        ConfigRevision, ProviderInstanceId, ProviderKind, PublicModelId, UpstreamModelId,
        snapshot::SnapshotControl,
    },
};

mod accounts;
mod auth;
mod catalog;
mod client_keys;
mod observability;
mod openai;
mod settings;
mod system;
mod wire;
mod xai;

pub(super) struct AdminTestFixture {
    pub services: AdminServices,
    pub auth: Arc<MemoryAuthStore>,
    pub settings: Arc<MemorySettingsStore>,
}

impl AdminTestFixture {
    pub async fn new() -> Self {
        let api_key = Arc::new(Mutex::new(None));
        let auth = Arc::new(MemoryAuthStore::new(api_key.clone()));
        let settings = Arc::new(MemorySettingsStore::new(api_key));
        let client_keys = Arc::new(MemoryClientKeyStore);
        let unused = Arc::new(UnusedStore);
        let stores = AdminStorePorts::new(
            unused.clone(),
            auth.clone(),
            unused.clone(),
            client_keys.clone(),
            unused,
            settings.clone(),
        );
        let providers: Vec<Arc<dyn ProviderAdmin>> = vec![
            Arc::new(UnusedProvider::new("openai")),
            Arc::new(UnusedProvider::new("xai")),
        ];
        let bundle = gateway_admin::initialize(
            AdminConfig {
                session_ttl_minutes: 60,
                default_username: "admin_1".to_owned(),
                default_password: InitialAdminPassword::new("strong-admin-password"),
            },
            stores,
            providers,
            Arc::new(NoopSnapshot),
            Arc::new(NoopProbe),
            Arc::new(UnusedSystem),
        )
        .await
        .expect("initialize test admin services");
        Self {
            services: bundle.services(),
            auth,
            settings,
        }
    }

    pub fn state(&self) -> AdminTestState {
        AdminTestState(self.services.clone())
    }
}

#[derive(Clone)]
pub(super) struct AdminTestState(AdminServices);

impl AdminSessionState for AdminTestState {
    fn admin_services(&self) -> &AdminServices {
        &self.0
    }
}

pub(super) struct MemoryAuthStore {
    password_hash: Mutex<Option<String>>,
    sessions: Mutex<BTreeMap<String, AdminSession>>,
    failures: Mutex<BTreeMap<String, u32>>,
    audits: Mutex<Vec<AdminAuditEvent>>,
    api_key: Arc<Mutex<Option<AdminApiKey>>>,
    fail_audit: AtomicBool,
}

impl MemoryAuthStore {
    fn new(api_key: Arc<Mutex<Option<AdminApiKey>>>) -> Self {
        Self {
            password_hash: Mutex::new(None),
            sessions: Mutex::new(BTreeMap::new()),
            failures: Mutex::new(BTreeMap::new()),
            audits: Mutex::new(Vec::new()),
            api_key,
            fail_audit: AtomicBool::new(false),
        }
    }

    pub fn insert_session(&self, session_id: &str) {
        self.sessions.lock().expect("sessions").insert(
            session_id.to_owned(),
            AdminSession {
                admin_user_id: "admin_1".to_owned(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        );
    }

    pub fn set_api_key(&self, value: &str) {
        *self.api_key.lock().expect("API key") = Some(AdminApiKey::new(value));
    }

    pub fn fail_audit(&self, fail: bool) {
        self.fail_audit.store(fail, Ordering::SeqCst);
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().expect("sessions").len()
    }

    pub fn audit_count(&self) -> usize {
        self.audits.lock().expect("audits").len()
    }
}

#[async_trait]
impl AuthStore for MemoryAuthStore {
    async fn load_password_hash(&self, _: &str) -> AdminStoreResult<Option<String>> {
        Ok(self.password_hash.lock().expect("password hash").clone())
    }

    async fn create_password_hash_if_absent(
        &self,
        _: &str,
        password_hash: &str,
    ) -> AdminStoreResult<bool> {
        let mut stored = self.password_hash.lock().expect("password hash");
        if stored.is_some() {
            return Ok(false);
        }
        *stored = Some(password_hash.to_owned());
        Ok(true)
    }

    async fn load_admin_api_key(&self) -> AdminStoreResult<Option<AdminApiKey>> {
        Ok(self.api_key.lock().expect("API key").clone())
    }

    async fn load_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        Ok(self
            .sessions
            .lock()
            .expect("sessions")
            .get(session_id)
            .cloned())
    }

    async fn store_session(
        &self,
        session_id: &str,
        session: &AdminSession,
    ) -> AdminStoreResult<()> {
        self.sessions
            .lock()
            .expect("sessions")
            .insert(session_id.to_owned(), session.clone());
        Ok(())
    }

    async fn delete_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        Ok(self.sessions.lock().expect("sessions").remove(session_id))
    }

    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        _: u64,
    ) -> AdminStoreResult<bool> {
        Ok(self
            .failures
            .lock()
            .expect("failures")
            .get(source)
            .is_some_and(|count| *count >= failure_limit))
    }

    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        _: u64,
    ) -> AdminStoreResult<bool> {
        let mut failures = self.failures.lock().expect("failures");
        let count = failures.entry(source.to_owned()).or_default();
        *count = count.saturating_add(1);
        Ok(*count >= failure_limit)
    }

    async fn clear_login_failures(&self, source: &str) -> AdminStoreResult<()> {
        self.failures.lock().expect("failures").remove(source);
        Ok(())
    }

    async fn append_audit_event(&self, event: AdminAuditEvent) -> AdminStoreResult<()> {
        if self.fail_audit.load(Ordering::SeqCst) {
            return Err(unavailable("auth audit"));
        }
        self.audits.lock().expect("audits").push(event);
        Ok(())
    }
}

pub(super) struct MemorySettingsStore {
    settings: Mutex<RuntimeSettings>,
    api_key: Arc<Mutex<Option<AdminApiKey>>>,
}

impl MemorySettingsStore {
    fn new(api_key: Arc<Mutex<Option<AdminApiKey>>>) -> Self {
        Self {
            settings: Mutex::new(test_runtime_settings()),
            api_key,
        }
    }

    pub fn set_api_key(&self, value: &str) {
        *self.api_key.lock().expect("API key") = Some(AdminApiKey::new(value));
    }
}

#[async_trait]
impl SettingsStore for MemorySettingsStore {
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Ok(self.settings.lock().expect("settings").clone())
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Ok(self.api_key.lock().expect("API key").is_some())
    }

    async fn replace_runtime_settings(
        &self,
        command: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        let updated = RuntimeSettings {
            config_revision: next_revision(command.expected_config_revision),
            provider_model_mappings: command.provider_model_mappings,
            refresh_margin_seconds: command.refresh_margin_seconds,
            refresh_concurrency: command.refresh_concurrency,
            max_concurrent_per_account: command.max_concurrent_per_account,
            request_interval_ms: command.request_interval_ms,
            rotation_strategy: command.rotation_strategy,
            usage_retention_days: command.usage_retention_days,
            ops_event_retention_days: command.ops_event_retention_days,
            audit_retention_days: command.audit_retention_days,
            updated_at: Utc::now(),
        };
        *self.settings.lock().expect("settings") = updated.clone();
        Ok(updated)
    }

    async fn replace_admin_api_key(
        &self,
        expected_config_revision: Revision,
        key: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        *self.api_key.lock().expect("API key") = Some(key);
        Ok(AdminApiKeyMutation {
            config_revision: next_revision(expected_config_revision),
            exists: true,
        })
    }

    async fn delete_admin_api_key(
        &self,
        expected_config_revision: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        *self.api_key.lock().expect("API key") = None;
        Ok(AdminApiKeyMutation {
            config_revision: next_revision(expected_config_revision),
            exists: false,
        })
    }
}

pub(super) struct MemoryClientKeyStore;

#[async_trait]
impl ClientKeyStore for MemoryClientKeyStore {
    async fn list_client_keys(&self, _: ClientKeyListQuery) -> AdminStoreResult<ClientKeyPage> {
        Ok(ClientKeyPage {
            config_revision: Revision::new(1).expect("revision"),
            items: Vec::new(),
            total: 0,
            next_cursor: None,
        })
    }

    async fn reveal_client_key(
        &self,
        id: &ClientApiKeyId,
    ) -> AdminStoreResult<Option<ClientKeySecret>> {
        let now = Utc::now();
        Ok(Some(ClientKeySecret::new(
            ClientKeyRecord {
                id: id.clone(),
                name: "revealed".to_owned(),
                label: None,
                provider_kind: ProviderKind::new("openai").expect("provider kind"),
                prefix: "sk_aaaaaaaaa".to_owned(),
                enabled: true,
                limits: RateLimits::unlimited(),
                last_used_at: None,
                created_at: now,
                updated_at: now,
            },
            format!("sk_{}", "a".repeat(43)),
        )))
    }

    async fn create_client_key(
        &self,
        _: NewClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        Err(unavailable("client key create"))
    }

    async fn update_client_key(
        &self,
        _: UpdateClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        Err(unavailable("client key update"))
    }

    async fn set_client_key_enabled(
        &self,
        _: SetClientKeyEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)> {
        Err(unavailable("client key enabled"))
    }

    async fn delete_client_key(
        &self,
        _: DeleteClientKey,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable("client key delete"))
    }
}

struct UnusedStore;

#[async_trait]
impl AccountStore for UnusedStore {
    async fn list_accounts(&self, _: AccountListQuery) -> AdminStoreResult<AccountPage> {
        Err(unavailable("account list"))
    }

    async fn load_account(&self, _: &str) -> AdminStoreResult<Option<AccountRecord>> {
        Err(unavailable("account"))
    }

    async fn load_account_usage(
        &self,
        _: TimeRange,
        _: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>> {
        Err(unavailable("account usage"))
    }

    async fn list_credentials(
        &self,
        _: &ProviderKind,
        _: CredentialListQuery,
    ) -> AdminStoreResult<CredentialPage> {
        Err(unavailable("credential list"))
    }

    async fn credential_details(
        &self,
        _: &ProviderKind,
        _: &ProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>> {
        Err(unavailable("credential"))
    }

    async fn load_credentials_for_export(
        &self,
        _: &ProviderKind,
        _: &[ProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>> {
        Err(unavailable("credential export"))
    }

    async fn commit_credential_import(
        &self,
        _: CredentialImportCommit,
        _: &MutationContext,
    ) -> AdminStoreResult<CredentialImportResult> {
        Err(unavailable("credential import"))
    }

    async fn commit_authorization(
        &self,
        _: AuthorizationCommit,
        _: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        Err(unavailable("authorization"))
    }

    async fn commit_credential_rotation(
        &self,
        _: CredentialRotationCommit,
        _: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        Err(unavailable("credential rotation"))
    }

    async fn commit_credential_refresh(
        &self,
        _: CredentialRotationCommit,
        _: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        Err(unavailable("credential refresh"))
    }

    async fn set_account_enabled(
        &self,
        _: SetAccountEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable("account enabled"))
    }

    async fn delete_account(
        &self,
        _: DeleteAccount,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable("account delete"))
    }

    async fn record_credential_export(
        &self,
        _: &[ProviderAccountId],
        _: &MutationContext,
    ) -> AdminStoreResult<()> {
        Err(unavailable("credential export audit"))
    }
}

#[async_trait]
impl CatalogStore for UnusedStore {
    async fn list_provider_instances(&self, _: bool) -> AdminStoreResult<ProviderInstanceCatalog> {
        Err(unavailable("provider catalog"))
    }

    async fn load_provider_instance(
        &self,
        _: &ProviderInstanceId,
    ) -> AdminStoreResult<Option<ProviderInstanceDetail>> {
        Err(unavailable("provider instance"))
    }

    async fn create_provider_instance(
        &self,
        _: CreateProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(unavailable("provider create"))
    }

    async fn update_provider_instance(
        &self,
        _: UpdateProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(unavailable("provider update"))
    }

    async fn set_provider_instance_enabled(
        &self,
        _: SetProviderInstanceEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<ProviderInstanceMutation> {
        Err(unavailable("provider enabled"))
    }

    async fn delete_provider_instance(
        &self,
        _: DeleteProviderInstance,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unavailable("provider delete"))
    }
}

#[async_trait]
impl ObservabilityStore for UnusedStore {
    async fn dashboard_summary(&self, _: TimeRange) -> AdminStoreResult<DashboardObservation> {
        Err(unavailable("dashboard"))
    }

    async fn dashboard_trend(&self, _: TimeRange) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Err(unavailable("dashboard trend"))
    }

    async fn usage_trend(
        &self,
        _: TimeRange,
        _: UsageFilter,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Err(unavailable("usage trend"))
    }

    async fn list_usage_records(&self, _: UsageQuery) -> AdminStoreResult<UsagePage> {
        Ok(UsagePage {
            items: Vec::new(),
            total: 0,
            next_cursor: None,
        })
    }

    async fn usage_record_detail(&self, _: &str) -> AdminStoreResult<UsageDetail> {
        Err(unavailable("usage detail"))
    }

    async fn usage_summary(&self, _: TimeRange, _: UsageFilter) -> AdminStoreResult<UsageOverview> {
        Err(unavailable("usage summary"))
    }

    async fn usage_diagnostics(
        &self,
        _: TimeRange,
        _: UsageFilter,
        _: DiagnosticDimension,
    ) -> AdminStoreResult<Vec<DiagnosticObservation>> {
        Err(unavailable("usage diagnostics"))
    }

    async fn list_ops_errors(&self, _: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        Err(unavailable("ops errors"))
    }
}

struct UnusedProvider {
    kind: ProviderKind,
}

impl UnusedProvider {
    fn new(kind: &str) -> Self {
        Self {
            kind: ProviderKind::new(kind).expect("provider kind"),
        }
    }
}

#[async_trait]
impl ProviderAdmin for UnusedProvider {
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }

    async fn account_unavailable(&self, _: &ProviderAccountId) {}

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
        _: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn start_authorization(
        &self,
        _: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn complete_authorization(
        &self,
        _: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn prepare_rotation(
        &self,
        _: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn prepare_refresh(
        &self,
        _: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn quota(
        &self,
        _: &ProviderAccountId,
        _: bool,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn models(
        &self,
        _: &ProviderAccountId,
        _: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        Err(unsupported_provider())
    }

    async fn export_credentials(
        &self,
        _: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        Err(unsupported_provider())
    }
}

struct NoopSnapshot;

impl SnapshotControl for NoopSnapshot {
    fn publish_committed(&self, _: ConfigRevision) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

struct NoopProbe;

impl AccountProbe for NoopProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, GatewayError>> {
        Box::pin(async { panic!("unused account probe") })
    }
}

struct UnusedSystem;

#[async_trait]
impl SystemOperations for UnusedSystem {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn update_detail(&self, _: bool) -> Result<SystemUpdateDetail, SystemOperationError> {
        Err(unavailable_system())
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        Box::pin(futures::stream::empty())
    }

    async fn perform_update(
        &self,
        _: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }
}

fn test_runtime_settings() -> RuntimeSettings {
    let mappings: ProviderModelMappings = BTreeMap::from([
        (
            ProviderKind::new("openai").expect("provider kind"),
            BTreeMap::from([(
                PublicModelId::new("coding-default").expect("public model"),
                UpstreamModelId::new("gpt-5.4").expect("upstream model"),
            )]),
        ),
        (
            ProviderKind::new("xai").expect("provider kind"),
            BTreeMap::from([(
                PublicModelId::new("grok-latest").expect("public model"),
                UpstreamModelId::new("grok-4.5").expect("upstream model"),
            )]),
        ),
    ]);
    RuntimeSettings {
        config_revision: Revision::new(7).expect("revision"),
        provider_model_mappings: mappings,
        refresh_margin_seconds: 3_600,
        refresh_concurrency: 2,
        max_concurrent_per_account: 3,
        request_interval_ms: 50,
        rotation_strategy: RotationStrategy::Smart,
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
        updated_at: Utc::now(),
    }
}

fn next_revision(revision: Revision) -> Revision {
    Revision::new(revision.get().saturating_add(1)).expect("next revision")
}

fn unavailable(resource: &'static str) -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        resource,
        "unused test port",
    )
}

fn unsupported_provider() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unsupported)
}

fn unavailable_system() -> SystemOperationError {
    SystemOperationError::new(SystemOperationErrorKind::Internal, "unused test operation")
}
