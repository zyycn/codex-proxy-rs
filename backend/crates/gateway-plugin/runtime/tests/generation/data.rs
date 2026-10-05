//! 验证插件事实查询与额度刷新回调的参数和能力边界

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        AdminError, MutationContext, Revision, accounts::AccountRecord,
        plugins::management::PluginManagementRequest, provider_credentials::*,
    },
    ports::{plugin_accounts::PluginAccountAccess, plugin_management::PluginManagement},
};
use gateway_core::{
    account::{AccountWeight, CredentialState, ProviderAccountId, QuotaState},
    routing::{ConfigRevision, ProviderKind},
    runtime::extensions::ExtensionPreparationPort,
};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use serde_json::{Value, json};

#[derive(Default)]
struct Facts(AtomicUsize, AtomicUsize, Option<String>);

fn account() -> AccountRecord {
    let at = "2026-01-01T00:00:00Z".parse().unwrap();
    AccountRecord {
        id: "acct_facts".into(),
        provider_kind: ProviderKind::new("openai").unwrap(),
        groups: vec![gateway_admin::model::account_groups::AccountGroupRef {
            id: gateway_core::routing::AccountGroupId::new("grp_11111111111111111111111111111111")
                .unwrap(),
            name: "private group label".into(),
            color: gateway_admin::model::account_groups::AccountGroupColor::parse("#112233FF")
                .unwrap(),
            enabled: true,
        }],
        name: "private name".into(),
        notes: Some("private notes".into()),
        email: Some("private@example.invalid".into()),
        upstream_user_id: None,
        upstream_account_id: None,
        plan_type: Some("private plan".into()),
        authentication_kind: "oauth".into(),
        credential_revision: Revision::new(1).unwrap(),
        has_refresh_token: true,
        access_token_expires_at: None,
        next_refresh_at: None,
        enabled: true,
        concurrency_limit: None,
        weight: AccountWeight::new(1).unwrap(),
        model_access: Default::default(),
        outbound_proxy: None,
        credential_state: CredentialState::Ready,
        credential_observed_at: at,
        quota: QuotaState::unknown(),
        last_error_reason: None,
        last_error_message: Some("private diagnostic".into()),
        created_at: at,
        updated_at: at,
    }
}

#[async_trait]
impl PluginAccountAccess for Facts {
    async fn refresh_quota(
        &self,
        account: &ProviderAccountId,
    ) -> Result<ProviderQuota, AdminError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        if account.as_str() == "acct_missing" {
            return Err(AdminError::not_found("missing account"));
        }
        if account.as_str() == "acct_unsupported" {
            return Err(AdminError::invalid("unsupported quota refresh"));
        }
        if account.as_str() == "acct_unavailable" {
            return Err(AdminError::unavailable("quota refresh unavailable"));
        }
        let mut quota = self.get_quota(account).await?;
        quota.observed_at = Some("2026-01-01T00:00:00Z".parse().unwrap());
        quota.windows[0].used_percent = Some(17.5);
        Ok(quota)
    }

    async fn list(&self, query: PluginAccountListQuery) -> Result<PluginAccountPage, AdminError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(query.limit.get(), 1);
        assert_eq!(query.provider_kind.unwrap().as_str(), "openai");
        assert_eq!(query.cursor.unwrap().as_str(), "acct_before");
        let mut account = account();
        account.email.clone_from(&self.2);
        Ok(PluginAccountPage {
            accounts: vec![account],
            next_cursor: Some(ProviderAccountId::new("acct_facts").unwrap()),
        })
    }
    async fn get_runtime(&self, _: &ProviderAccountId) -> Result<AccountRecord, AdminError> {
        panic!("事实回调只使用对应的窄查询")
    }
    async fn get_credential(
        &self,
        _: &ProviderAccountId,
    ) -> Result<PluginAccountCredential, AdminError> {
        Err(AdminError::not_found("fixture credential is unavailable"))
    }
    async fn save(
        &self,
        _: PreparedPluginAccountSave,
        _: &MutationContext,
    ) -> Result<PluginAccountSaveResult, AdminError> {
        panic!("不允许修改账号")
    }
    async fn get_quota(&self, account: &ProviderAccountId) -> Result<ProviderQuota, AdminError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(account.as_str(), "acct_facts");
        Ok(ProviderQuota {
            credits: None,
            plan_type: Some("private plan".into()),
            observed_at: None,
            refresh_token_expires_at: None,
            windows: vec![ProviderQuotaWindow {
                key: "weekly".into(),
                group: "private group".into(),
                label: "private label".into(),
                limit_id: None,
                limit_name: None,
                role: None,
                local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
                window_seconds: Some(604800),
                used_percent: None,
                reset_at: None,
                limit_reached: false,
                local_usage: None,
                provider_data: None,
            }],
            limit_reached: false,
            provider_data: None,
        })
    }
}

#[tokio::test]
async fn management_facts_validate_queries_without_permission_declarations() {
    for email in [Some("private@example.invalid"), None] {
        let (cache, store, runtime) = super::setup_with_contributions_and_restart_circuit(
            Contributions::from([crate::support::contribution(
                Capability::Management,
                vec![Stage::Management],
                vec![],
                vec![],
            )]),
            Default::default(),
        )
        .await;
        let facts = Arc::new(Facts(
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            email.map(str::to_owned),
        ));
        let port: Arc<dyn PluginAccountAccess> = facts.clone();
        runtime.bind_account_ports(&port).unwrap();
        {
            let mut snapshot = store.snapshot.lock().unwrap();
            let instance = &mut snapshot.instances[0];
            instance.configuration = json!({
                "management_registration":{"routes":[{"method":"GET","path":"facts","request_content_types":[],"response_content_types":["application/json"]}]},
                "data_queries":[
                    {"method":"host.data.accounts.list","query":{"provider_id":"openai","cursor":"acct_before","limit":1}},
                    {"method":"host.data.quota.get","query":{"account_id":"acct_facts"}},
                    {"method":"host.data.accounts.list","query":{"limit":0}},
                    {"method":"host.data.accounts.list","query":{"limit":65535}},
                    {"method":"host.data.quota.get","query":{"account_id":"acct_facts","sql":"SELECT secret"}}
                ]
            });
        }
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        assert!(runtime.middleware_registry().resolve(&generation).is_none());
        assert!(runtime.policy_registry().resolve(&generation).is_none());
        let view = runtime.views(&generation).await.unwrap().remove(0);
        let response = runtime
            .handle(
                &generation,
                &view.target,
                PluginManagementRequest {
                    headers: Vec::new(),
                    method: "GET".into(),
                    path: "facts".into(),
                    query: String::new(),
                    content_type: None,
                    body: vec![],
                    request_id: "data-fixture".into(),
                },
            )
            .await
            .unwrap();
        let result: Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(facts.0.load(Ordering::SeqCst), 2, "{result}");
        assert_eq!(
            result[0],
            json!({"schema_version":1,"accounts":[{
                "account_id":"acct_facts","provider_id":"openai","group_ids":["grp_11111111111111111111111111111111"],
                "name":"private name","email":email,
                "enabled":true,"updated_at_ms":1767225600000i64
            }],"next_cursor":"acct_facts"})
        );
        let page: gateway_plugin_sdk::call::data::AccountFactsPage =
            serde_json::from_value(result[0].clone()).unwrap();
        assert_eq!(page.accounts[0].name, "private name");
        assert_eq!(page.accounts[0].email.as_deref(), email);
        assert_eq!(
            result[1],
            json!({"schema_version":1,"account_id":"acct_facts","observed_at_ms":null,
                "windows":[{"key":"weekly","window_seconds":604800,"used_percent":null,"reset_at_ms":null}]})
        );
        for invalid in &result.as_array().unwrap()[2..] {
            assert_eq!(invalid["error"], "invalid_input");
        }

        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}

#[tokio::test]
async fn quota_refresh_is_available_without_permission_declarations() {
    {
        let (cache, store, runtime) = super::setup_with_contributions_and_restart_circuit(
            Contributions::from([crate::support::contribution(
                Capability::Management,
                vec![Stage::Management],
                vec![],
                vec![],
            )]),
            Default::default(),
        )
        .await;
        let facts = Arc::new(Facts::default());
        let port: Arc<dyn PluginAccountAccess> = facts.clone();
        runtime.bind_account_ports(&port).unwrap();
        {
            let mut snapshot = store.snapshot.lock().unwrap();
            let instance = &mut snapshot.instances[0];
            instance.configuration = json!({
                "management_registration":{"routes":[{"method":"GET","path":"facts","request_content_types":[],"response_content_types":["application/json"]}]},
                "data_queries":[
                    {"method":"host.data.quota.get","query":{"account_id":"acct_facts"}},
                    {"method":"host.quota_observations.refresh","query":{"account_id":"acct_facts"}},
                    {"method":"host.quota_observations.refresh","query":{"account_id":"acct_facts","provider_id":"forged"}},
                    {"method":"host.quota_observations.refresh","query":{"account_id":"acct_missing"}},
                    {"method":"host.quota_observations.refresh","query":{"account_id":"acct_unsupported"}},
                    {"method":"host.quota_observations.refresh","query":{"account_id":"acct_unavailable"}},
                    {"method":"host.auth.get","query":{"account_id":"acct_facts"}}

                ]
            });
        }
        let generation =
            ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
                .await
                .unwrap();
        assert!(runtime.middleware_registry().resolve(&generation).is_none());
        assert!(runtime.policy_registry().resolve(&generation).is_none());
        let view = runtime.views(&generation).await.unwrap().remove(0);
        let response = runtime
            .handle(
                &generation,
                &view.target,
                PluginManagementRequest {
                    headers: Vec::new(),
                    method: "GET".into(),
                    path: "facts".into(),
                    query: String::new(),
                    content_type: None,
                    body: vec![],
                    request_id: "data-fixture".into(),
                },
            )
            .await
            .unwrap();
        let result: Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(facts.1.load(Ordering::SeqCst), 4);
        assert_eq!(result[0]["observed_at_ms"], Value::Null);
        assert_eq!(
            result[1],
            json!({"schema_version":1,"account_id":"acct_facts","observed_at_ms":1767225600000i64,
                "windows":[{"key":"weekly","window_seconds":604800,"used_percent":17.5,"reset_at_ms":null}]})
        );
        for (index, code) in [
            (2, "invalid_input"),
            (3, "rejected"),
            (4, "invalid_input"),
            (5, "fault"),
            (6, "rejected"),
        ] {
            assert_eq!(result[index]["error"], code);
        }

        drop(generation);
        runtime.shutdown().await;
        super::wait_until_empty(cache.path()).await;
    }
}
