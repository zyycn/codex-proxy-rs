use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures::future::BoxFuture;
use gateway_admin::{
    AdminConfig, AdminServices, InitialAdminPassword,
    model::{
        MutationContext, Revision,
        account_groups::{
            AccountGroupAccountSummary, AccountGroupCapacity, AccountGroupColor,
            AccountGroupListQuery, AccountGroupMutation, AccountGroupPage, AccountGroupRecord,
            AccountGroupUsage, DeleteAccountGroup, NewAccountGroup, SetAccountGroupEnabled,
            UpdateAccountGroup,
        },
        accounts::{
            AccountListQuery, AccountPage, AccountPageItem, AccountUpdateResult, AccountUsage,
            AccountUsageWindowQuery, AccountUsageWindowResult, AccountsUpdateResult,
            BatchUpdateAccounts, DeleteAccounts, UpdateAccount,
        },
        auth::{AdminAuditEvent, AdminSession},
        client_keys::{
            ClientKeyListQuery, ClientKeyPage, ClientKeyRecord, ClientKeySecret, DeleteClientKey,
            NewClientKey, SetClientKeyEnabled, UpdateClientKey,
        },
        observability::{
            DashboardObservation, DecimalAmount, DiagnosticDimension, DiagnosticObservation,
            OpsError, OpsErrorPage, OpsErrorQuery, RequestMetricPoint, TimeRange, UsageDetail,
            UsageFilter, UsageOverview, UsagePage, UsageQuery, UsageRecord,
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
            AdminApiKey, AdminApiKeyMutation, ModelMappings, ReplaceRuntimeSettings,
            RotationStrategy, RuntimeSettings,
        },
        system::{SystemOperationAccepted, SystemUpdateDetail, SystemUpdateStatus, SystemVersion},
    },
    ports::{
        provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind},
        store::{
            AccountGroupStore, AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStorePorts,
            AdminStoreResult, AuthStore, ClientKeyStore, ObservabilityStore, SettingsStore,
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
        probe::{AccountProbe, AccountProbeError, AccountProbeRequest, AccountProbeResult},
    },
    policy::{ClientApiKeyId, RateLimits},
    routing::{
        ConfigRevision, ProviderKind, PublicModelId, UpstreamModelId, snapshot::SnapshotControl,
    },
};

mod account_groups;
mod accounts;
mod auth;
mod client_keys;
mod observability;
mod settings;
mod system;
mod wire;

pub(super) struct AdminTestFixture {
    pub services: AdminServices,
    pub auth: Arc<MemoryAuthStore>,
    pub settings: Arc<MemorySettingsStore>,
    pub usage_records: Arc<Mutex<Vec<UsageRecord>>>,
    pub usage_detail: Arc<Mutex<Option<UsageDetail>>>,
    pub diagnostics: Arc<Mutex<Vec<DiagnosticObservation>>>,
    pub ops_errors: Arc<Mutex<Vec<OpsError>>>,
    pub dashboard_observation: Arc<Mutex<Option<DashboardObservation>>>,
    pub dashboard_summary_range: Arc<Mutex<Option<TimeRange>>>,
}

impl AdminTestFixture {
    pub async fn new() -> Self {
        Self::with_system(Arc::new(UnusedSystem)).await
    }

    pub async fn with_system(system: Arc<dyn SystemOperations>) -> Self {
        let api_key = Arc::new(Mutex::new(None));
        let auth = Arc::new(MemoryAuthStore::new(api_key.clone()));
        let settings = Arc::new(MemorySettingsStore::new(api_key));
        let client_keys = Arc::new(MemoryClientKeyStore);
        let account_groups = Arc::new(MemoryAccountGroupStore::new());
        let usage_records = Arc::new(Mutex::new(Vec::new()));
        let usage_detail = Arc::new(Mutex::new(None));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let ops_errors = Arc::new(Mutex::new(Vec::new()));
        let dashboard_observation = Arc::new(Mutex::new(None));
        let dashboard_summary_range = Arc::new(Mutex::new(None));
        let unused = Arc::new(UnusedStore {
            usage_records: Arc::clone(&usage_records),
            usage_detail: Arc::clone(&usage_detail),
            diagnostics: Arc::clone(&diagnostics),
            ops_errors: Arc::clone(&ops_errors),
            dashboard_observation: Arc::clone(&dashboard_observation),
            dashboard_summary_range: Arc::clone(&dashboard_summary_range),
        });
        let stores = AdminStorePorts::new(
            unused.clone(),
            account_groups.clone(),
            auth.clone(),
            client_keys.clone(),
            unused,
            settings.clone(),
            gateway_admin::ports::backup::BackupStorePorts::disabled(),
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
            system,
        )
        .await
        .expect("initialize test admin services");
        Self {
            services: bundle.services(),
            auth,
            settings,
            usage_records,
            usage_detail,
            diagnostics,
            ops_errors,
            dashboard_observation,
            dashboard_summary_range,
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
    audits: Mutex<Vec<AdminAuditEvent>>,
    api_key: Arc<Mutex<Option<AdminApiKey>>>,
    fail_audit: AtomicBool,
}

impl MemoryAuthStore {
    fn new(api_key: Arc<Mutex<Option<AdminApiKey>>>) -> Self {
        Self {
            password_hash: Mutex::new(None),
            sessions: Mutex::new(BTreeMap::new()),
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
        let mut settings = self.settings.lock().expect("settings");
        let updated = RuntimeSettings {
            config_revision: next_revision(settings.config_revision),
            model_mappings: command.model_mappings,
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
        *settings = updated.clone();
        Ok(updated)
    }

    async fn replace_admin_api_key(
        &self,
        key: AdminApiKey,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        *self.api_key.lock().expect("API key") = Some(key);
        let revision = self.settings.lock().expect("settings").config_revision;
        Ok(AdminApiKeyMutation {
            config_revision: next_revision(revision),
            exists: true,
        })
    }

    async fn delete_admin_api_key(
        &self,
        _: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        *self.api_key.lock().expect("API key") = None;
        let revision = self.settings.lock().expect("settings").config_revision;
        Ok(AdminApiKeyMutation {
            config_revision: next_revision(revision),
            exists: false,
        })
    }
}

pub(super) struct MemoryClientKeyStore;

pub(super) const PRIMARY_GROUP_ID: &str = "grp_11111111111111111111111111111111";
pub(super) const SECONDARY_GROUP_ID: &str = "grp_22222222222222222222222222222222";

struct MemoryAccountGroupState {
    revision: Revision,
    groups: BTreeMap<gateway_core::routing::AccountGroupId, AccountGroupRecord>,
}

pub(super) struct MemoryAccountGroupStore {
    state: Mutex<MemoryAccountGroupState>,
}

impl MemoryAccountGroupStore {
    fn new() -> Self {
        let now = Utc::now();
        let primary_id = group_id(PRIMARY_GROUP_ID);
        let secondary_id = group_id(SECONDARY_GROUP_ID);
        let groups = BTreeMap::from([
            (
                primary_id.clone(),
                AccountGroupRecord {
                    id: primary_id,
                    name: "Alpha routing".to_owned(),
                    description: Some("Primary traffic".to_owned()),
                    color: group_color("#2563ebff"),
                    enabled: true,
                    member_count: 2,
                    provider_counts: BTreeMap::from([
                        ("openai".to_owned(), 1),
                        ("xai".to_owned(), 1),
                    ]),
                    client_key_count: 2,
                    account_summary: account_summary(1, 1, 2),
                    capacity: capacity(Some(0), 1),
                    usage: usage("1.25", "5.5"),
                    created_at: now,
                    updated_at: now,
                },
            ),
            (
                secondary_id.clone(),
                AccountGroupRecord {
                    id: secondary_id,
                    name: "Beta routing".to_owned(),
                    description: None,
                    color: group_color("#64748B80"),
                    enabled: false,
                    member_count: 0,
                    provider_counts: BTreeMap::new(),
                    client_key_count: 0,
                    account_summary: account_summary(0, 0, 0),
                    capacity: capacity(Some(0), 0),
                    usage: usage("0", "0"),
                    created_at: now,
                    updated_at: now,
                },
            ),
        ]);
        Self {
            state: Mutex::new(MemoryAccountGroupState {
                revision: Revision::new(7).expect("revision"),
                groups,
            }),
        }
    }
}

#[async_trait]
impl AccountGroupStore for MemoryAccountGroupStore {
    async fn list_account_groups(
        &self,
        query: AccountGroupListQuery,
    ) -> AdminStoreResult<AccountGroupPage> {
        let state = self.state.lock().expect("account groups");
        let search = query.search.as_ref().map(|value| value.to_lowercase());
        let mut matching = state
            .groups
            .values()
            .filter(|record| {
                query
                    .enabled
                    .is_none_or(|enabled| record.enabled == enabled)
            })
            .filter(|record| {
                search.as_ref().is_none_or(|search| {
                    record.name.to_lowercase().contains(search)
                        || record
                            .description
                            .as_ref()
                            .is_some_and(|description| description.to_lowercase().contains(search))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        matching.sort_by(|left, right| left.name.cmp(&right.name));
        let total = matching.len() as u64;
        let page_size = usize::from(query.page_size.get());
        let offset = usize::try_from(query.page.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .saturating_mul(page_size);
        let items = matching.into_iter().skip(offset).take(page_size).collect();
        Ok(AccountGroupPage {
            config_revision: state.revision,
            items,
            total,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn create_account_group(
        &self,
        command: NewAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let mut state = self.state.lock().expect("account groups");
        let now = Utc::now();
        let record = AccountGroupRecord {
            id: command.id.clone(),
            name: command.name,
            description: command.description,
            color: command.color,
            enabled: true,
            member_count: 0,
            provider_counts: BTreeMap::new(),
            client_key_count: 0,
            account_summary: account_summary(0, 0, 0),
            capacity: capacity(Some(0), 0),
            usage: usage("0", "0"),
            created_at: now,
            updated_at: now,
        };
        state.groups.insert(command.id.clone(), record);
        mutation(&mut state, command.id, true)
    }

    async fn update_account_group(
        &self,
        command: UpdateAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let mut state = self.state.lock().expect("account groups");
        let record = state
            .groups
            .get_mut(&command.id)
            .ok_or_else(|| not_found("account group"))?;
        record.name = command.name;
        record.description = command.description;
        record.color = command.color;
        record.updated_at = Utc::now();
        mutation(&mut state, command.id, true)
    }

    async fn set_account_group_enabled(
        &self,
        command: SetAccountGroupEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let mut state = self.state.lock().expect("account groups");
        let record = state
            .groups
            .get_mut(&command.id)
            .ok_or_else(|| not_found("account group"))?;
        record.enabled = command.enabled;
        record.updated_at = Utc::now();
        mutation(&mut state, command.id, true)
    }

    async fn delete_account_group(
        &self,
        command: DeleteAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let mut state = self.state.lock().expect("account groups");
        if state.groups.remove(&command.id).is_none() {
            return Err(not_found("account group"));
        }
        mutation(&mut state, command.id, false)
    }
}

fn group_id(value: &str) -> gateway_core::routing::AccountGroupId {
    gateway_core::routing::AccountGroupId::new(value).expect("account group ID")
}

fn mutation(
    state: &mut MemoryAccountGroupState,
    id: gateway_core::routing::AccountGroupId,
    include_record: bool,
) -> AdminStoreResult<AccountGroupMutation> {
    state.revision = next_revision(state.revision);
    Ok(AccountGroupMutation {
        config_revision: state.revision,
        record: include_record.then(|| state.groups.get(&id).expect("group record").clone()),
        id,
    })
}

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
                groups: Vec::new(),
                provider_kinds: vec![ProviderKind::new("openai").expect("provider kind")],
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

struct UnusedStore {
    usage_records: Arc<Mutex<Vec<UsageRecord>>>,
    usage_detail: Arc<Mutex<Option<UsageDetail>>>,
    diagnostics: Arc<Mutex<Vec<DiagnosticObservation>>>,
    ops_errors: Arc<Mutex<Vec<OpsError>>>,
    dashboard_observation: Arc<Mutex<Option<DashboardObservation>>>,
    dashboard_summary_range: Arc<Mutex<Option<TimeRange>>>,
}

#[async_trait]
impl AccountStore for UnusedStore {
    async fn list_accounts(&self, _: AccountListQuery) -> AdminStoreResult<AccountPage> {
        Err(unavailable("account list"))
    }

    async fn load_account(&self, _: &str) -> AdminStoreResult<Option<AccountPageItem>> {
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
impl ObservabilityStore for UnusedStore {
    async fn dashboard_summary(
        &self,
        range: TimeRange,
        _: DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation> {
        *self
            .dashboard_summary_range
            .lock()
            .expect("dashboard summary range") = Some(range);
        self.dashboard_observation
            .lock()
            .expect("dashboard observation")
            .clone()
            .ok_or_else(|| unavailable("dashboard"))
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
        let items = self.usage_records.lock().expect("usage records").clone();
        Ok(UsagePage {
            total: items.len() as u64,
            items,
            next_cursor: None,
        })
    }

    async fn usage_record_detail(&self, _: &str) -> AdminStoreResult<UsageDetail> {
        self.usage_detail
            .lock()
            .expect("usage detail")
            .clone()
            .ok_or_else(|| unavailable("usage detail"))
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
        Ok(self.diagnostics.lock().expect("diagnostics").clone())
    }

    async fn list_ops_errors(&self, _: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        let items = self.ops_errors.lock().expect("ops errors").clone();
        Ok(OpsErrorPage {
            total: items.len() as u64,
            items,
            next_cursor: None,
        })
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

    fn connection_test_operation(
        &self,
        _: &gateway_core::routing::UpstreamModelId,
        _: &str,
    ) -> Result<gateway_core::operation::Operation, ProviderAdminError> {
        Err(unsupported_provider())
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
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
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
    let mappings: ModelMappings = BTreeMap::from([
        (
            PublicModelId::new("coding-default").expect("public model"),
            UpstreamModelId::new("gpt-5.4").expect("upstream model"),
        ),
        (
            PublicModelId::new("grok-latest").expect("public model"),
            UpstreamModelId::new("grok-4.5").expect("upstream model"),
        ),
    ]);
    RuntimeSettings {
        config_revision: Revision::new(7).expect("revision"),
        model_mappings: mappings,
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

fn account_summary(available: u64, limited: u64, total: u64) -> AccountGroupAccountSummary {
    AccountGroupAccountSummary {
        available,
        limited,
        total,
    }
}

fn group_color(value: &str) -> AccountGroupColor {
    AccountGroupColor::parse(value).expect("group color")
}

fn capacity(used_slots: Option<u64>, total_slots: u64) -> AccountGroupCapacity {
    AccountGroupCapacity {
        used_slots,
        total_slots,
    }
}

fn usage(today_usd: &str, total_usd: &str) -> AccountGroupUsage {
    AccountGroupUsage {
        today_usd: today_usd.parse::<DecimalAmount>().expect("today cost"),
        total_usd: total_usd.parse::<DecimalAmount>().expect("total cost"),
    }
}

fn unavailable(resource: &'static str) -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        resource,
        "unused test port",
    )
}

fn not_found(resource: &'static str) -> AdminStoreError {
    AdminStoreError::new(AdminStoreErrorKind::NotFound, resource, "not found")
}

fn unsupported_provider() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unsupported)
}

fn unavailable_system() -> SystemOperationError {
    SystemOperationError::new(SystemOperationErrorKind::Internal, "unused test operation")
}
