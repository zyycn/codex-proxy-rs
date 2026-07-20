mod accounts;
mod auth;
mod catalog;
mod client_keys;
mod observability;
mod openai;
mod settings;
mod system;
mod xai;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
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
            DashboardDesktopRelease, DashboardObservation, DashboardWireProfile,
            DashboardWireTarget, DesktopReleaseStatus, DiagnosticDimension, DiagnosticObservation,
            OpsErrorPage, OpsErrorQuery, RequestMetricPoint, TimeRange, UsageDetail, UsageFilter,
            UsageOverview, UsagePage, UsageQuery,
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
        settings::{AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RuntimeSettings},
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
use gateway_core::{
    engine::{
        credential::ProviderAccountId,
        probe::{AccountProbe, AccountProbeRequest, AccountProbeResult},
    },
    error::{GatewayError, GatewayErrorKind},
    policy::ClientApiKeyId,
    routing::{ConfigRevision, ProviderInstanceId, ProviderKind, snapshot::SnapshotControl},
};

pub(super) struct AdminHarness {
    default_password: String,
    accounts: Arc<dyn AccountStore>,
    auth: Arc<dyn AuthStore>,
    catalog: Arc<dyn CatalogStore>,
    client_keys: Arc<dyn ClientKeyStore>,
    observability: Arc<dyn ObservabilityStore>,
    settings: Arc<dyn SettingsStore>,
    providers: Vec<Arc<dyn ProviderAdmin>>,
    probe: Arc<dyn AccountProbe>,
    system: Arc<dyn SystemOperations>,
}

impl AdminHarness {
    pub(super) fn new() -> Self {
        let unavailable = Arc::new(UnavailableStore);
        Self {
            default_password: "strong-test-password".to_owned(),
            accounts: unavailable.clone(),
            auth: Arc::new(BootstrapAuthStore::default()),
            catalog: unavailable.clone(),
            client_keys: unavailable.clone(),
            observability: unavailable.clone(),
            settings: unavailable,
            providers: vec![
                Arc::new(UnavailableProvider::new("openai")),
                Arc::new(UnavailableProvider::new("xai")),
            ],
            probe: Arc::new(UnavailableProbe),
            system: Arc::new(UnavailableSystem),
        }
    }

    pub(super) fn default_password(mut self, password: &str) -> Self {
        self.default_password = password.to_owned();
        self
    }

    pub(super) fn accounts(mut self, store: Arc<dyn AccountStore>) -> Self {
        self.accounts = store;
        self
    }

    pub(super) fn auth(mut self, store: Arc<dyn AuthStore>) -> Self {
        self.auth = store;
        self
    }

    pub(super) fn catalog(mut self, store: Arc<dyn CatalogStore>) -> Self {
        self.catalog = store;
        self
    }

    pub(super) fn client_keys(mut self, store: Arc<dyn ClientKeyStore>) -> Self {
        self.client_keys = store;
        self
    }

    pub(super) fn observability(mut self, store: Arc<dyn ObservabilityStore>) -> Self {
        self.observability = store;
        self
    }

    pub(super) fn settings(mut self, store: Arc<dyn SettingsStore>) -> Self {
        self.settings = store;
        self
    }

    pub(super) fn provider(mut self, provider: Arc<dyn ProviderAdmin>) -> Self {
        self.providers
            .retain(|registered| registered.provider_kind() != provider.provider_kind());
        self.providers.push(provider);
        self
    }

    pub(super) fn probe(mut self, probe: Arc<dyn AccountProbe>) -> Self {
        self.probe = probe;
        self
    }

    pub(super) fn system(mut self, system: Arc<dyn SystemOperations>) -> Self {
        self.system = system;
        self
    }

    pub(super) async fn build(self) -> AdminServices {
        gateway_admin::initialize(
            AdminConfig {
                session_ttl_minutes: 60,
                default_username: "admin".to_owned(),
                default_password: InitialAdminPassword::new(self.default_password),
            },
            AdminStorePorts::new(
                self.accounts,
                self.auth,
                self.catalog,
                self.client_keys,
                self.observability,
                self.settings,
            ),
            self.providers,
            Arc::new(NoopSnapshot),
            self.probe,
            self.system,
        )
        .await
        .expect("initialize admin test harness")
        .services()
    }
}

#[derive(Default)]
struct BootstrapAuthStore {
    password_hash: Mutex<Option<String>>,
}

#[async_trait]
impl AuthStore for BootstrapAuthStore {
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
        Ok(None)
    }

    async fn load_session(&self, _: &str) -> AdminStoreResult<Option<AdminSession>> {
        Ok(None)
    }

    async fn store_session(&self, _: &str, _: &AdminSession) -> AdminStoreResult<()> {
        Err(unavailable("admin session"))
    }

    async fn delete_session(&self, _: &str) -> AdminStoreResult<Option<AdminSession>> {
        Err(unavailable("admin session"))
    }

    async fn login_source_is_throttled(&self, _: &str, _: u32, _: u64) -> AdminStoreResult<bool> {
        Ok(false)
    }

    async fn record_login_failure(&self, _: &str, _: u32, _: u64) -> AdminStoreResult<bool> {
        Err(unavailable("login failure"))
    }

    async fn clear_login_failures(&self, _: &str) -> AdminStoreResult<()> {
        Ok(())
    }

    async fn append_audit_event(&self, _: AdminAuditEvent) -> AdminStoreResult<()> {
        Ok(())
    }
}

struct UnavailableStore;

#[async_trait]
impl AccountStore for UnavailableStore {
    async fn list_accounts(&self, _: AccountListQuery) -> AdminStoreResult<AccountPage> {
        Err(unavailable("accounts"))
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
        Err(unavailable("credentials"))
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
impl CatalogStore for UnavailableStore {
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
impl ClientKeyStore for UnavailableStore {
    async fn list_client_keys(&self, _: ClientKeyListQuery) -> AdminStoreResult<ClientKeyPage> {
        Err(unavailable("client key list"))
    }

    async fn reveal_client_key(
        &self,
        _: &ClientApiKeyId,
    ) -> AdminStoreResult<Option<ClientKeySecret>> {
        Err(unavailable("client key"))
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

#[async_trait]
impl ObservabilityStore for UnavailableStore {
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
        Err(unavailable("usage records"))
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

#[async_trait]
impl SettingsStore for UnavailableStore {
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings> {
        Err(unavailable("settings"))
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        Err(unavailable("admin API key"))
    }

    async fn replace_runtime_settings(
        &self,
        _: ReplaceRuntimeSettings,
        _: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings> {
        Err(unavailable("settings"))
    }

    async fn replace_admin_api_key(
        &self,
        _: Revision,
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unavailable("admin API key"))
    }

    async fn delete_admin_api_key(
        &self,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unavailable("admin API key"))
    }
}

struct UnavailableProvider {
    kind: ProviderKind,
    dashboard_profile: Option<DashboardWireProfile>,
}

impl UnavailableProvider {
    fn new(kind: &str) -> Self {
        Self {
            kind: ProviderKind::new(kind).expect("provider kind"),
            dashboard_profile: None,
        }
    }
}

#[async_trait]
impl ProviderAdmin for UnavailableProvider {
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }

    async fn account_unavailable(&self, _: &ProviderAccountId) {}

    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile> {
        self.dashboard_profile.clone()
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

pub(super) fn dashboard_profile_provider() -> Arc<dyn ProviderAdmin> {
    Arc::new(UnavailableProvider {
        kind: ProviderKind::new("openai").expect("provider kind"),
        dashboard_profile: Some(DashboardWireProfile {
            originator: "gateway-admin-test".to_owned(),
            codex_version: "test".to_owned(),
            desktop_version: "test".to_owned(),
            desktop_build: "test".to_owned(),
            target: DashboardWireTarget {
                os_type: "linux".to_owned(),
                os_version: "test".to_owned(),
                arch: "x86_64".to_owned(),
                terminal: "test".to_owned(),
            },
            user_agent: "gateway-admin-test".to_owned(),
            verified_at: chrono::Utc::now(),
            release: DashboardDesktopRelease {
                status: DesktopReleaseStatus::Unchecked,
                checked_at: None,
                latest_version: None,
                latest_build: None,
                published_at: None,
                minimum_system_version: None,
                hardware_requirements: None,
                download_url: None,
                download_size: None,
                signature_present: None,
                error: None,
            },
        }),
    })
}

struct NoopSnapshot;

impl SnapshotControl for NoopSnapshot {
    fn publish_committed(&self, _: ConfigRevision) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

struct UnavailableProbe;

impl AccountProbe for UnavailableProbe {
    fn probe(
        &self,
        _: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "test account probe is unavailable",
            ))
        })
    }
}

struct UnavailableSystem;

#[async_trait]
impl SystemOperations for UnavailableSystem {
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

fn unavailable(resource: &'static str) -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        resource,
        "unavailable in this test",
    )
}

fn unsupported_provider() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unsupported)
}

fn unavailable_system() -> SystemOperationError {
    SystemOperationError::new(
        SystemOperationErrorKind::Internal,
        "unavailable in this test",
    )
}
