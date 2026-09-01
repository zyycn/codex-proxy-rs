mod account_groups;
mod accounts;
mod auth;
mod backup;
mod client_keys;
mod observability;
mod openai;
mod settings;
mod system;
mod xai;

use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_admin::{
    AdminConfig, AdminServices, InitialAdminPassword,
    model::{
        MutationContext, Revision,
        account_groups::{
            AccountGroupListQuery, AccountGroupMemberFact, AccountGroupMutation, AccountGroupPage,
            DeleteAccountGroup, NewAccountGroup, SetAccountGroupEnabled, UpdateAccountGroup,
        },
        accounts::{
            AccountListQuery, AccountPage, AccountRuntimeSnapshot, AccountUpdateResult,
            AccountUsage, AccountUsageWindowQuery, AccountUsageWindowResult, AccountsUpdateResult,
            BatchUpdateAccounts, DeleteAccounts, UpdateAccount,
        },
        auth::{AdminAuditEvent, AdminSession},
        client_distribution::CodexDesktopWindowsDownloads,
        client_keys::{
            ClientKeyListQuery, ClientKeyPage, ClientKeyRecord, ClientKeySecret, DeleteClientKey,
            NewClientKey, SetClientKeyEnabled, UpdateClientKey,
        },
        observability::{
            DashboardDesktopRelease, DashboardObservation, DashboardWireAttribute,
            DashboardWireProfile, DashboardWireTarget, DesktopReleaseStatus, DiagnosticDimension,
            DiagnosticObservation, OpsErrorPage, OpsErrorQuery, RequestMetricPoint, TimeRange,
            UsageDetail, UsageFilter, UsageOverview, UsagePage, UsageQuery,
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
        backup::BackupStorePorts,
        client_distribution::ClientDistributionResolver,
        provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind},
        store::{
            AccountGroupStore, AccountRuntimeStore, AccountStore, AdminAccountStorePorts,
            AdminStoreError, AdminStoreErrorKind, AdminStorePorts, AdminStoreResult, AuthStore,
            ClientKeyStore, ObservabilityStore, SettingsStore,
        },
        system::{
            SystemOperationError, SystemOperationErrorKind, SystemOperations,
            SystemUpdateEventStream,
        },
    },
};
use gateway_core::{
    account::ProviderAccountId,
    engine::probe::{AccountProbe, AccountProbeError, AccountProbeRequest, AccountProbeResult},
    error::{GatewayError, GatewayErrorKind},
    policy::ClientApiKeyId,
    routing::{ConfigRevision, ProviderKind, snapshot::SnapshotControl},
};

pub(super) struct AdminHarness {
    default_password: String,
    session_ttl_minutes: u64,
    accounts: Arc<dyn AccountStore>,
    account_runtime: Arc<dyn AccountRuntimeStore>,
    account_groups: Arc<dyn AccountGroupStore>,
    auth: Arc<dyn AuthStore>,
    client_keys: Arc<dyn ClientKeyStore>,
    observability: Arc<dyn ObservabilityStore>,
    settings: Arc<dyn SettingsStore>,
    backup: BackupStorePorts,
    providers: Vec<Arc<dyn ProviderAdmin>>,
    probe: Arc<dyn AccountProbe>,
    system: Arc<dyn SystemOperations>,
}

impl AdminHarness {
    pub(super) fn new() -> Self {
        let unavailable = Arc::new(UnavailableStore);
        Self {
            default_password: "strong-test-password".to_owned(),
            session_ttl_minutes: 60,
            accounts: unavailable.clone(),
            account_runtime: unavailable.clone(),
            account_groups: Arc::new(UnavailableAccountGroupStore),
            auth: Arc::new(BootstrapAuthStore::default()),
            client_keys: unavailable.clone(),
            observability: unavailable.clone(),
            settings: unavailable,
            backup: BackupStorePorts::disabled(),
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

    pub(super) fn session_ttl_minutes(mut self, minutes: u64) -> Self {
        self.session_ttl_minutes = minutes;
        self
    }

    pub(super) fn accounts(mut self, store: Arc<dyn AccountStore>) -> Self {
        self.accounts = store;
        self
    }

    pub(super) fn account_runtime(mut self, store: Arc<dyn AccountRuntimeStore>) -> Self {
        self.account_runtime = store;
        self
    }

    pub(super) fn account_groups(mut self, store: Arc<dyn AccountGroupStore>) -> Self {
        self.account_groups = store;
        self
    }

    pub(super) fn auth(mut self, store: Arc<dyn AuthStore>) -> Self {
        self.auth = store;
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

    pub(super) fn backup(mut self, backup: BackupStorePorts) -> Self {
        self.backup = backup;
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
                session_ttl_minutes: self.session_ttl_minutes,
                default_username: "admin".to_owned(),
                default_password: InitialAdminPassword::new(self.default_password),
            },
            AdminStorePorts::new(
                AdminAccountStorePorts::new(
                    self.accounts,
                    self.account_runtime,
                    self.account_groups,
                ),
                self.auth,
                self.client_keys,
                self.observability,
                self.settings,
                self.backup,
            ),
            self.providers,
            Arc::new(NoopSnapshot),
            self.probe,
            Arc::new(NoopClientDistribution),
            self.system,
        )
        .await
        .expect("initialize admin test harness")
        .services()
    }
}

struct NoopClientDistribution;

#[async_trait]
impl ClientDistributionResolver for NoopClientDistribution {
    async fn resolve_codex_desktop_windows(&self, _: bool) -> CodexDesktopWindowsDownloads {
        CodexDesktopWindowsDownloads {
            resolved_at: Utc::now(),
            cached: false,
            warning: None,
            packages: Vec::new(),
        }
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

    async fn append_audit_event(&self, _: AdminAuditEvent) -> AdminStoreResult<()> {
        Ok(())
    }
}

struct UnavailableStore;

struct UnavailableAccountGroupStore;

#[async_trait]
impl AccountGroupStore for UnavailableAccountGroupStore {
    async fn list_account_groups(
        &self,
        _: AccountGroupListQuery,
    ) -> AdminStoreResult<AccountGroupPage> {
        Err(unavailable("account groups"))
    }

    async fn load_account_group_members(
        &self,
        _: &[gateway_core::routing::AccountGroupId],
    ) -> AdminStoreResult<Vec<AccountGroupMemberFact>> {
        Err(unavailable("account group members"))
    }

    async fn create_account_group(
        &self,
        _: NewAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unavailable("account group create"))
    }

    async fn update_account_group(
        &self,
        _: UpdateAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unavailable("account group update"))
    }

    async fn set_account_group_enabled(
        &self,
        _: SetAccountGroupEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unavailable("account group state"))
    }

    async fn delete_account_group(
        &self,
        _: DeleteAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unavailable("account group delete"))
    }
}

#[async_trait]
impl AccountStore for UnavailableStore {
    async fn list_accounts(
        &self,
        _: AccountListQuery,
        _: AccountRuntimeSnapshot,
    ) -> AdminStoreResult<AccountPage> {
        Err(unavailable("accounts"))
    }

    async fn load_account(
        &self,
        _: &str,
        _: AccountRuntimeSnapshot,
    ) -> AdminStoreResult<Option<gateway_admin::model::accounts::AccountPageItem>> {
        Err(unavailable("account"))
    }

    async fn load_account_usage(
        &self,
        _: TimeRange,
        _: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>> {
        Err(unavailable("account usage"))
    }

    async fn load_account_usage_by_windows(
        &self,
        _: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>> {
        Err(unavailable("account quota window usage"))
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

    async fn update_account(
        &self,
        _: UpdateAccount,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        Err(unavailable("account enabled"))
    }

    async fn recover_account(
        &self,
        _: &ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult> {
        Err(unavailable("account recovery"))
    }

    async fn batch_update_accounts(
        &self,
        _: BatchUpdateAccounts,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountsUpdateResult> {
        Err(unavailable("account batch update"))
    }

    async fn delete_accounts(
        &self,
        _: DeleteAccounts,
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
impl AccountRuntimeStore for UnavailableStore {
    async fn active_rate_limits(&self) -> AdminStoreResult<AccountRuntimeSnapshot> {
        Ok(AccountRuntimeSnapshot::default())
    }

    async fn account_runtime(&self, _: &[String]) -> AdminStoreResult<AccountRuntimeSnapshot> {
        Ok(AccountRuntimeSnapshot::default())
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
    async fn dashboard_summary(
        &self,
        _: TimeRange,
        _: DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation> {
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

    async fn usage_calculated_billing_facts(
        &self,
        _: TimeRange,
        _: UsageFilter,
    ) -> AdminStoreResult<Vec<gateway_admin::model::observability::UsageCalculatedBillingFact>>
    {
        Err(unavailable("usage billing facts"))
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
        _: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unavailable("admin API key"))
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        Err(unavailable("admin API key"))
    }
}

struct UnavailableProvider {
    kind: ProviderKind,
    dashboard_profile: Option<DashboardWireProfile>,
    calculated_billing: Option<gateway_admin::model::observability::CalculatedBillingBreakdown>,
}

impl UnavailableProvider {
    fn new(kind: &str) -> Self {
        Self {
            kind: ProviderKind::new(kind).expect("provider kind"),
            dashboard_profile: None,
            calculated_billing: None,
        }
    }
}

#[async_trait]
impl ProviderAdmin for UnavailableProvider {
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }

    async fn account_unavailable(&self, _: &ProviderAccountId) {}

    fn connection_test_operation(
        &self,
        _: &gateway_core::routing::UpstreamModelId,
        _: &str,
    ) -> Result<gateway_core::operation::Operation, ProviderAdminError> {
        Err(unsupported_provider())
    }

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
        Ok(self.calculated_billing.clone())
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
        _: gateway_admin::model::provider_credentials::ProviderQuotaRequest,
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
            provider: "openai".to_owned(),
            product: "gateway-admin-test".to_owned(),
            version: "test".to_owned(),
            build: Some("test".to_owned()),
            target: DashboardWireTarget {
                os_type: "linux".to_owned(),
                os_version: "test".to_owned(),
                arch: "x86_64".to_owned(),
                terminal: "test".to_owned(),
            },
            user_agent: "gateway-admin-test".to_owned(),
            attributes: vec![DashboardWireAttribute {
                label: "Core".to_owned(),
                value: "test".to_owned(),
            }],
            verified_at: Some(chrono::Utc::now()),
            release: Some(DashboardDesktopRelease {
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
            }),
        }),
        calculated_billing: None,
    })
}

pub(super) fn calculated_billing_provider() -> Arc<dyn ProviderAdmin> {
    let amount = |value| gateway_admin::model::observability::CurrencyCost {
        currency: "USD".to_owned(),
        amount: gateway_admin::model::observability::DecimalAmount::from_str(value)
            .expect("test billing amount"),
    };
    Arc::new(UnavailableProvider {
        kind: ProviderKind::new("openai").expect("provider kind"),
        dashboard_profile: None,
        calculated_billing: Some(
            gateway_admin::model::observability::CalculatedBillingBreakdown {
                input_amount: amount("0.8"),
                output_amount: amount("0.2"),
                cache_read_amount: amount("0"),
                cache_write_amount: amount("0"),
                standard_amount: amount("1"),
                total_amount: amount("1.25"),
                input_price_per_million: amount("0"),
                output_price_per_million: amount("0"),
                cache_read_price_per_million: amount("0"),
                cache_write_price_per_million: amount("0"),
                service_tier: None,
                multiplier_percent: 125,
            },
        ),
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
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "test account probe is unavailable",
            )
            .into())
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
