use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use gateway_admin::{
    AdminConfig, ClientConfig, InitialAdminPassword,
    model::{
        AdminError, MutationActor, MutationContext,
        client_distribution::CodexDesktopWindowsDownloads,
        plugins::{
            PluginSource,
            distribution::{
                DownloadedPlugin, GithubReleaseQuery, PluginRelease, PluginUpdateSource,
                RemotePluginLocation, SourceCredential,
            },
            instances::{
                PluginCapabilityBinding, PluginFailurePolicy, PluginFrontendIdentityBinding,
                PluginInstance, PluginPermissionGrant,
            },
        },
        pricing::PricingSyncPreview,
        proxies::ProxyTestResult,
        system::{SystemOperationAccepted, SystemUpdateDetail, SystemUpdateStatus, SystemVersion},
    },
    ports::{
        client_distribution::ClientDistributionResolver,
        plugin_accounts::PluginAccountAccess,
        plugins::{PluginDistribution, PluginPackageInspector},
        pricing::PricingSource,
        proxy::ProxyProbe,
        system::{SystemOperationError, SystemOperations, SystemUpdateEventStream},
    },
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRuntime, PluginRuntimeConfig, RpcLimits,
};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use gateway_store::{StoreBundle, StoreConfig};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

pub struct Environment {
    pub store: StoreBundle,
    pub directory: tempfile::TempDir,
    admin: sqlx::PgPool,
    schema: String,
    plugin_accounts: std::sync::Mutex<BTreeMap<usize, Arc<dyn PluginAccountAccess>>>,
}

impl Environment {
    pub async fn client_key(&self, id: &str, plaintext: &str) {
        self.client_key_with_limits(id, plaintext, gateway_core::policy::RateLimits::unlimited())
            .await;
    }

    pub async fn client_key_with_limits(
        &self,
        id: &str,
        plaintext: &str,
        limits: gateway_core::policy::RateLimits,
    ) {
        self.client_key_with_limits_and_groups(id, plaintext, limits, Vec::new())
            .await;
    }

    pub async fn client_key_with_limits_and_groups(
        &self,
        id: &str,
        plaintext: &str,
        limits: gateway_core::policy::RateLimits,
        group_ids: Vec<gateway_core::routing::AccountGroupId>,
    ) {
        use gateway_admin::model::client_keys::NewClientKey;
        use gateway_core::{engine::budget::ClientBudgetLimits, policy::ClientApiKeyId};

        self.store
            .admin_ports()
            .client_keys()
            .create_client_key(
                NewClientKey {
                    request_profile_overrides: BTreeMap::new(),
                    id: ClientApiKeyId::new(id).unwrap(),
                    name: format!("fixture {id}"),
                    label: None,
                    group_ids,
                    limits,
                    budget: ClientBudgetLimits::default(),
                    plaintext: plaintext.to_owned(),
                },
                &mutation(),
            )
            .await
            .unwrap();
    }

    pub async fn account_group_with_account(
        &self,
        account: &gateway_core::account::ProviderAccountId,
    ) -> gateway_core::routing::AccountGroupId {
        use gateway_admin::model::account_groups::{AccountGroupColor, NewAccountGroup};
        use gateway_core::routing::AccountGroupId;

        let id = AccountGroupId::new(format!("grp_{}", uuid::Uuid::new_v4().simple())).unwrap();
        self.store
            .admin_ports()
            .account_groups()
            .create_account_group(
                NewAccountGroup {
                    disable_fast: false,
                    id: id.clone(),
                    name: format!("fixture {}", id.as_str()),
                    description: None,
                    color: AccountGroupColor::parse("#2563EBFF").unwrap(),
                },
                &mutation(),
            )
            .await
            .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into {}.account_group_accounts
             (account_group_id, provider_account_id, created_at) values ($1, $2, now())",
            self.schema
        )))
        .bind(id.as_str())
        .bind(account.as_str())
        .execute(&self.admin)
        .await
        .unwrap();
        id
    }

    pub async fn set_account_group_enabled(
        &self,
        id: gateway_core::routing::AccountGroupId,
        enabled: bool,
    ) {
        use gateway_admin::model::account_groups::SetAccountGroupEnabled;

        self.store
            .admin_ports()
            .account_groups()
            .set_account_group_enabled(SetAccountGroupEnabled { id, enabled }, &mutation())
            .await
            .unwrap();
    }

    pub async fn plugin(
        &self,
        configuration: serde_json::Value,
        grants: Vec<PluginPermissionGrant>,
    ) -> (Arc<PluginRuntime>, gateway_core::CoreBundle) {
        self.install_plugin(configuration, grants).await;
        self.runtime().await
    }

    pub async fn install_plugin(
        &self,
        configuration: serde_json::Value,
        grants: Vec<PluginPermissionGrant>,
    ) {
        let plugin_id = configuration
            .get("plugin_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| super::DEFAULT_PLUGIN_ID.to_owned());
        let permissions = grants
            .iter()
            .map(|grant| serde_json::from_value(json!(grant.permission)).unwrap())
            .collect();
        let mut contributes = Contributions::new();
        if configuration.get("maintenance_fixture").is_some() {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::Maintenance,
                vec![Stage::Maintenance],
                vec![],
                vec![],
            )]);
        }

        if configuration.get("command_registration").is_some() {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::CommandLine,
                vec![Stage::CommandLine],
                vec![],
                vec![],
            )]);
        }
        if configuration.get("management_registration").is_some() {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::Management,
                vec![Stage::Management],
                vec![],
                vec![],
            )]);
        }
        let has_middleware = configuration.get("middleware_marker").is_some();
        if has_middleware {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::Middleware,
                vec![Stage::Request],
                vec!["openai".into()],
                vec!["openai".into()],
            )]);
        }
        let frontend_authentication = configuration
            .get("frontend_authentication_result")
            .is_some()
            .then(|| {
                (
                    configuration["frontend_authentication_principal"]
                        .as_str()
                        .unwrap_or("fixture-principal")
                        .to_owned(),
                    configuration["frontend_authentication_client_key_id"]
                        .as_str()
                        .expect("frontend auth fixture client key ID")
                        .to_owned(),
                    configuration["frontend_authentication_exclusive"]
                        .as_bool()
                        .unwrap_or(false),
                )
            });
        if frontend_authentication.is_some() {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::FrontendAuthentication,
                vec![Stage::Authentication],
                vec![],
                vec![],
            )]);
        }
        let has_model_router = configuration.get("route_decision").is_some()
            || configuration.get("nested_model_fixture").is_some()
            || configuration.get("affinity_fixture").is_some();
        if has_model_router {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::ModelRouter,
                vec![Stage::Routing],
                vec![],
                vec![],
            )]);
        }
        let has_scheduler = configuration.get("schedule_decision").is_some()
            || configuration.get("schedule_pick_index").is_some()
            || configuration.get("schedule_fault").is_some();
        if has_scheduler {
            contributes.extend([super::contribution_for_id(
                &plugin_id,
                Capability::Scheduler,
                vec![Stage::Scheduling],
                vec![],
                vec![],
            )]);
        }
        let archive = super::package_with_contributions_for_id(
            super::worker(),
            &plugin_id,
            permissions,
            contributes,
        );
        let inspector = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap());
        let artifact = inspector
            .inspect(Arc::clone(&archive), None)
            .await
            .unwrap_or_else(|error| {
                let validation = gateway_plugin_runtime::ValidatedPackage::read(
                    archive,
                    None,
                    PackageLimits::default(),
                )
                .err();
                panic!("测试插件包校验失败：{error:?}; {validation:?}");
            });
        let store = self.store.admin_ports().plugins();
        let mutation = mutation();
        let installed = store
            .install_artifact(artifact, PluginSource::Upload, &mutation)
            .await
            .unwrap();
        let installed = store
            .accept_artifact(&installed.artifact.metadata.sha256, &mutation)
            .await
            .unwrap();
        let grants = installed
            .artifact
            .metadata
            .requested_permissions
            .iter()
            .cloned()
            .map(|permission| PluginPermissionGrant { permission })
            .collect();
        let instance = PluginInstance {
            id: uuid::Uuid::new_v4().to_string(),
            name: "provider".into(),
            artifact_sha256: installed.artifact.metadata.sha256,
            enabled: true,
            trusted_process: true,
            configuration,
            secrets: BTreeMap::new(),
            grants,
            bindings: [
                has_model_router.then(|| PluginCapabilityBinding {
                    contribution: format!("{plugin_id}.modelRouter"),
                    stage: "routing".into(),
                    order: 0,
                    failure_policy: PluginFailurePolicy::Reject,
                    client_key_ids: vec![],
                    account_group_ids: vec![],
                    provider_ids: vec![],
                    models: vec![],
                    identity_bindings: vec![],
                }),
                has_scheduler.then(|| PluginCapabilityBinding {
                    contribution: format!("{plugin_id}.scheduler"),
                    stage: "scheduling".into(),
                    order: 0,
                    failure_policy: PluginFailurePolicy::Reject,
                    client_key_ids: vec![],
                    account_group_ids: vec![],
                    provider_ids: vec![],
                    models: vec![],
                    identity_bindings: vec![],
                }),
                frontend_authentication
                    .as_ref()
                    .map(|(principal, key_id, exclusive)| PluginCapabilityBinding {
                        contribution: format!("{plugin_id}.frontendAuthentication"),
                        stage: "authentication".into(),
                        order: 0,
                        failure_policy: if *exclusive {
                            PluginFailurePolicy::Reject
                        } else {
                            PluginFailurePolicy::Delegate
                        },
                        client_key_ids: vec![],
                        account_group_ids: vec![],
                        provider_ids: vec![],
                        models: vec![],
                        identity_bindings: vec![PluginFrontendIdentityBinding {
                            principal: principal.clone(),
                            client_key_id: key_id.clone(),
                        }],
                    }),
                has_middleware.then(|| PluginCapabilityBinding {
                    contribution: format!("{plugin_id}.middleware"),
                    stage: "request".into(),
                    order: 0,
                    failure_policy: PluginFailurePolicy::Reject,
                    client_key_ids: vec![],
                    account_group_ids: vec![],
                    provider_ids: vec![],
                    models: vec![],
                    identity_bindings: vec![],
                }),
            ]
            .into_iter()
            .flatten()
            .collect(),
            revision: installed.config_revision,
        };
        store
            .save_instance(instance, installed.config_revision, &mutation)
            .await
            .unwrap();
    }

    pub async fn runtime(&self) -> (Arc<PluginRuntime>, gateway_core::CoreBundle) {
        let runtime = self.plugin_runtime();
        let core = gateway_core::prepare(
            self.store.core_ports(),
            super::native::provider_registry(),
            Some(runtime.clone()),
            Some(runtime.observer_registry()),
            Some(runtime.policy_registry()),
            Some(runtime.middleware_registry()),
            Some(runtime.frontend_authentication_registry()),
        );
        let access = gateway_admin::initialize_plugin_accounts(
            super::native::admin_registry(),
            self.store.admin_ports().accounts(),
            core.snapshot_control(),
        );
        runtime.bind_account_ports(&access).unwrap();
        self.plugin_accounts
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&runtime) as usize, access);
        let core = core.activate().await.unwrap();
        runtime
            .bind_model_ports(
                &core.nested_model_execution_port(),
                &core.affinity_lookup_port(),
            )
            .unwrap();
        (runtime, core)
    }

    pub async fn command_plane(
        &self,
    ) -> (Arc<PluginRuntime>, gateway_core::CoreCommandPlaneBundle) {
        let runtime = self.plugin_runtime();
        let core = gateway_core::prepare_command_plane(
            self.store.core_ports(),
            super::native::provider_registry(),
            Some(runtime.clone()),
            Some(runtime.observer_registry()),
            Some(runtime.policy_registry()),
            Some(runtime.middleware_registry()),
        )
        .activate()
        .await
        .unwrap();
        runtime
            .bind_model_ports(
                &core.nested_model_execution_port(),
                &core.affinity_lookup_port(),
            )
            .unwrap();
        (runtime, core)
    }

    fn plugin_runtime(&self) -> Arc<PluginRuntime> {
        Arc::new(
            PluginRuntime::new(
                self.store.admin_ports().plugins(),
                self.store.admin_ports().plugin_state(),
                PluginRuntimeConfig {
                    cache_directory: self.directory.path().join("cache"),
                    host_version: "1.0.0".parse().unwrap(),
                    package_limits: PackageLimits::default(),
                    rpc_limits: RpcLimits::default(),
                    restart_circuit: Default::default(),
                },
                Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
                Arc::new(gateway_host::process::ProcessSupervisor::new(
                    std::num::NonZeroUsize::new(128).unwrap(),
                )),
            )
            .with_oauth_pending(self.store.provider_ports().oauth_pending())
            .with_network_policy(
                gateway_host::outbound::NetworkPolicy::new(&[
                    "127.0.0.0/8".into(),
                    "::1/128".into(),
                ])
                .unwrap(),
            ),
        )
    }

    pub async fn bind_admin_accounts(
        &self,
        runtime: &Arc<PluginRuntime>,
        core: &gateway_core::CoreBundle,
    ) -> gateway_admin::AdminBundle {
        let unavailable = Arc::new(UnusedAdminRuntime);
        let access = self
            .plugin_accounts
            .lock()
            .unwrap()
            .remove(&(Arc::as_ptr(runtime) as usize))
            .expect("runtime account access");
        gateway_admin::initialize_with_plugin_accounts(
            AdminConfig {
                session_ttl_minutes: 60,
                default_username: "plugin-test-admin".to_owned(),
                default_password: InitialAdminPassword::new("plugin-test-password"),
            },
            ClientConfig::default(),
            self.store.admin_ports(),
            gateway_admin::AdminRuntimePorts {
                plugin_preparation: runtime.clone(),
                plugin_management: runtime.clone(),
                published_snapshot: core.snapshots(),
                plugin_distribution: unavailable.clone(),
                plugin_inspector: Arc::new(PackageInspector::new(
                    PackageLimits::default(),
                    "1.0.0".parse().unwrap(),
                )),
                pricing_source: unavailable.clone(),
                providers: super::native::admin_registry(),
                snapshot: core.snapshot_control(),
                account_probe: core.account_probe(),
                proxy_probe: unavailable.clone(),
                client_distribution: unavailable.clone(),
                system: unavailable,
                client_key_verifier: core.client_key_verifier(),
            },
            access,
        )
        .await
        .unwrap()
    }

    pub fn release_plugin_accounts(&self, runtime: &Arc<PluginRuntime>) {
        self.plugin_accounts
            .lock()
            .unwrap()
            .remove(&(Arc::as_ptr(runtime) as usize))
            .expect("runtime account access");
    }

    pub async fn account(
        &self,
        proxy: Option<gateway_core::account::OutboundProxy>,
    ) -> gateway_core::account::ProviderAccountId {
        self.account_for("openai", proxy).await
    }

    pub async fn account_for(
        &self,
        provider: &str,
        proxy: Option<gateway_core::account::OutboundProxy>,
    ) -> gateway_core::account::ProviderAccountId {
        self.account_for_with_concurrency(provider, proxy, None)
            .await
    }

    pub async fn account_for_with_concurrency(
        &self,
        provider: &str,
        proxy: Option<gateway_core::account::OutboundProxy>,
        maximum_concurrency: Option<u32>,
    ) -> gateway_core::account::ProviderAccountId {
        use gateway_core::{
            account::{
                AccountConcurrencyLimit, AccountWeight, CredentialRevision, CredentialState,
                NewProviderAccount, PlaintextCredential, ProviderAccount, ProviderAccountId,
                QuotaState,
            },
            routing::ProviderKind,
        };
        let id = ProviderAccountId::new(format!("acct_{}", uuid::Uuid::new_v4().simple())).unwrap();
        let account = ProviderAccount::new(
            id.clone(),
            ProviderKind::new(provider).unwrap(),
            "native account".into(),
            None,
            "api_key".into(),
            CredentialRevision::new(1).unwrap(),
            None,
        )
        .with_outbound_proxy(proxy)
        .with_scheduling(
            maximum_concurrency.and_then(AccountConcurrencyLimit::new),
            AccountWeight::DEFAULT,
        )
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        );
        self.store
            .provider_ports()
            .accounts()
            .create_account(NewProviderAccount {
                account,
                credential: PlaintextCredential::new(
                    json!({"key":"test-only"}).as_object().unwrap().clone(),
                ),
                model_access: None,
            })
            .await
            .unwrap();
        id
    }

    pub async fn create() -> Option<Self> {
        Self::create_with_store_mode(false).await
    }

    pub async fn create_command() -> Option<Self> {
        Self::create_with_store_mode(true).await
    }

    async fn create_with_store_mode(command_line: bool) -> Option<Self> {
        let database = std::env::var("CPR_PLUGIN_TEST_DATABASE_URL").ok()?;
        let redis = std::env::var("CPR_PLUGIN_TEST_REDIS_URL").expect("isolated plugin Redis URL");
        let schema = format!("cpr_plugin_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create schema {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let mut database = url::Url::parse(&database).unwrap();
        database
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let connection = |mut url: url::Url| {
            let password = url.password().expect("test password").to_owned();
            url.set_password(None).unwrap();
            json!({"url":url.as_str(), "password":password})
        };
        let mut config: StoreConfig = serde_json::from_value(json!({
            "database":connection(database), "redis":connection(url::Url::parse(&redis).unwrap()),
        }))
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        config.resolve_and_validate(directory.path()).unwrap();
        let store = if command_line {
            gateway_store::initialize_command_line(config)
                .await
                .unwrap()
        } else {
            gateway_store::initialize(config).await.unwrap()
        };
        // 迁移可能已创建默认行；该场景只测租约释放，明确关闭相邻请求的间隔限制。
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("insert into {schema}.runtime_settings(id, config_revision, request_interval_ms, updated_at) values (1,1,0,now()) on conflict (id) do update set request_interval_ms=0")))
            .execute(&admin).await.unwrap();
        Some(Self {
            store,
            directory,
            admin,
            schema,
            plugin_accounts: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn close(self) {
        drop(self.store);
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop schema {} cascade",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .unwrap();
        self.admin.close().await;
    }

    pub async fn audit_requests(&self, action: &str) -> Vec<String> {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select admin_request_id from {}.admin_audit_events where action = $1 order by created_at",
            self.schema
        )))
        .bind(action)
        .fetch_all(&self.admin)
        .await
        .unwrap()
    }

    pub async fn bound_model_requests(&self, client_key_id: &str) -> Vec<(String, String)> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select request_kind, subagent_kind from {}.model_requests
             where client_api_key_ref = $1 order by started_at, id",
            self.schema
        )))
        .bind(client_key_id)
        .fetch_all(&self.admin)
        .await
        .unwrap()
    }

    pub async fn wait_for_bound_model_requests(
        &self,
        client_key_id: &str,
        expected: usize,
    ) -> Vec<(String, String)> {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let requests = self.bound_model_requests(client_key_id).await;
                if requests.len() >= expected {
                    break requests;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
}

struct UnusedAdminRuntime;

#[async_trait]
impl PluginDistribution for UnusedAdminRuntime {
    fn validate_source(&self, _: &PluginUpdateSource) -> Result<(), AdminError> {
        Err(AdminError::unavailable("unused plugin distribution"))
    }

    fn validate_credential(&self, _: &SourceCredential) -> Result<(), AdminError> {
        Err(AdminError::unavailable("unused plugin distribution"))
    }

    async fn query_release(
        &self,
        _: GithubReleaseQuery,
        _: Vec<SourceCredential>,
        _: Option<gateway_admin::model::plugins::distribution::PluginDistributionEgress>,
    ) -> Result<PluginRelease, AdminError> {
        Err(AdminError::unavailable("unused plugin distribution"))
    }

    async fn download(
        &self,
        _: RemotePluginLocation,
        _: Vec<SourceCredential>,
        _: Option<gateway_admin::model::plugins::distribution::PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError> {
        Err(AdminError::unavailable("unused plugin distribution"))
    }
}

#[async_trait]
impl PricingSource for UnusedAdminRuntime {
    async fn fetch(&self) -> Result<PricingSyncPreview, AdminError> {
        Err(AdminError::unavailable("unused pricing source"))
    }
}

#[async_trait]
impl ProxyProbe for UnusedAdminRuntime {
    async fn test(&self, _: &gateway_core::account::OutboundProxy, _: bool) -> ProxyTestResult {
        panic!("unexpected proxy probe")
    }
}

#[async_trait]
impl ClientDistributionResolver for UnusedAdminRuntime {
    async fn resolve_codex_desktop_windows(&self, _: bool) -> CodexDesktopWindowsDownloads {
        CodexDesktopWindowsDownloads {
            resolved_at: chrono::Utc::now(),
            cached: false,
            warning: None,
            packages: Vec::new(),
        }
    }
}

#[async_trait]
impl SystemOperations for UnusedAdminRuntime {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError> {
        Err(unused_system())
    }

    async fn update_detail(
        &self,
        _: bool,
        _: Option<gateway_admin::model::system::SystemUpdateChannel>,
    ) -> Result<SystemUpdateDetail, SystemOperationError> {
        Err(unused_system())
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        Box::pin(futures::stream::empty())
    }

    async fn perform_update(
        &self,
        _: Option<String>,
        _: Option<gateway_admin::model::system::SystemUpdateChannel>,
        _: Arc<dyn gateway_admin::ports::system::SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unused_system())
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        Err(unused_system())
    }

    async fn rollback(
        &self,
        _: Arc<dyn gateway_admin::ports::system::SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unused_system())
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unused_system())
    }
}

fn unused_system() -> SystemOperationError {
    SystemOperationError::new(
        gateway_admin::ports::system::SystemOperationErrorKind::Internal,
        "unused system operation",
    )
}

pub fn mutation() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "plugin-host-account-test".into(),
    }
}

pub fn account_grant(permission: &str) -> PluginPermissionGrant {
    PluginPermissionGrant {
        permission: permission.into(),
    }
}
