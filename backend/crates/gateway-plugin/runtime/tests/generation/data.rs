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
    routing::extensions::ExtensionPreparationPort,
    routing::{ConfigRevision, ProviderKind},
};
use gateway_plugin_sdk::{Capability, Contributions, Stage};
use serde_json::{Value, json};

#[derive(Default)]
struct Facts {
    reads: AtomicUsize,
    refreshes: AtomicUsize,
    email: Option<String>,
    runtime_account: Option<AccountRecord>,
}

fn account() -> AccountRecord {
    let at = "2026-01-01T00:00:00Z".parse().unwrap();
    AccountRecord {
        auto_use_quota_credits: false,
        outbound_proxy_id: None,
        outbound_proxy_enabled: true,
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
        self.refreshes.fetch_add(1, Ordering::SeqCst);
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
        self.reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(query.limit.get(), 1);
        assert_eq!(query.provider_kind.unwrap().as_str(), "openai");
        assert_eq!(query.cursor.unwrap().as_str(), "acct_before");
        let mut account = account();
        account.email.clone_from(&self.email);
        Ok(PluginAccountPage {
            accounts: vec![account],
            next_cursor: Some(ProviderAccountId::new("acct_facts").unwrap()),
        })
    }
    async fn get_runtime(&self, id: &ProviderAccountId) -> Result<AccountRecord, AdminError> {
        let account = self
            .runtime_account
            .as_ref()
            .expect("事实回调只使用对应的窄查询");
        if account.id != id.as_str() {
            return Err(AdminError::not_found("fixture account is unavailable"));
        }
        Ok(account.clone())
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
        self.reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(account.as_str(), "acct_facts");
        Ok(ProviderQuota {
            upstream: None,
            credits: Some(ProviderQuotaCredits {
                has_credits: true,
                unlimited: false,
                balance: Some("123456789.123456789".into()),
            }),
            plan_type: Some("private plan".into()),
            observed_at: None,
            refresh_token_expires_at: None,
            windows: vec![ProviderQuotaWindow {
                upstream: None,
                key: "weekly".into(),
                group: "private group".into(),
                label: "private label".into(),
                limit_id: None,
                limit_name: None,
                role: Some(ProviderQuotaWindowRole::Primary),
                local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
                window_seconds: Some(604800),
                used_percent: None,
                reset_at: None,
                limit_reached: true,
                local_usage: None,
                provider_data: None,
            }],
            limit_reached: true,
            provider_data: Some(ProviderDocument::new(
                gateway_core::account::OpaqueProviderData::new(
                    json!({"private":"provider-internal-fixture"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )),
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
        let facts = Arc::new(Facts {
            email: email.map(str::to_owned),
            ..Default::default()
        });
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
                    {"method":"host.data.quota.details","query":{"account_id":"acct_facts"}},
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
        assert!(
            runtime
                .execution_registry()
                .middleware(&generation)
                .is_none()
        );
        assert!(runtime.policy_registry().resolve(&generation).is_none());
        let view = runtime.views(&generation).await.unwrap().remove(0);
        let response = runtime
            .handle(
                &generation,
                &view.target,
                PluginManagementRequest {
                    request_context: None,
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

        assert_eq!(facts.reads.load(Ordering::SeqCst), 3, "{result}");
        assert_eq!(facts.refreshes.load(Ordering::SeqCst), 0);
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
        let details: gateway_plugin_sdk::call::data::QuotaDetails =
            serde_json::from_value(result[2].clone()).unwrap();
        assert_eq!(details.observed_at_ms, None);
        assert_eq!(details.windows[0].used_percent, None);
        assert_eq!(details.windows[0].role.as_deref(), Some("primary"));
        assert_eq!(details.windows[0].label, "private label");
        assert!(details.limit_reached && details.windows[0].limit_reached);
        assert_eq!(
            details.credits.unwrap().balance.as_deref(),
            Some("123456789.123456789")
        );
        assert!(!result[2].to_string().contains("provider-internal-fixture"));
        for invalid in &result.as_array().unwrap()[3..] {
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
        assert!(
            runtime
                .execution_registry()
                .middleware(&generation)
                .is_none()
        );
        assert!(runtime.policy_registry().resolve(&generation).is_none());
        let view = runtime.views(&generation).await.unwrap().remove(0);
        let response = runtime
            .handle(
                &generation,
                &view.target,
                PluginManagementRequest {
                    request_context: None,
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

        assert_eq!(facts.refreshes.load(Ordering::SeqCst), 4);
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

async fn management_queries(
    accounts: Option<Arc<dyn PluginAccountAccess>>,
    proxies: Option<Arc<dyn gateway_admin::ports::proxy::PluginProxyAccess>>,
    queries: Value,
) -> Value {
    let (cache, store, runtime) =
        super::setup_with_contributions(Contributions::from([crate::support::contribution(
            Capability::Management,
            vec![Stage::Management],
            vec![],
            vec![],
        )]))
        .await;
    if let Some(accounts) = &accounts {
        runtime.bind_account_ports(accounts).unwrap();
    }
    if let Some(proxies) = &proxies {
        runtime.bind_proxy_ports(proxies).unwrap();
    }
    store.snapshot.lock().unwrap().instances[0].configuration = json!({
        "management_registration":{"routes":[{"method":"GET","path":"facts","request_content_types":[],"response_content_types":["application/json"]}]},
        "data_queries":queries,
        "sdk_session":true,
    });
    let generation = ExtensionPreparationPort::prepare(&runtime, ConfigRevision::new(1).unwrap())
        .await
        .unwrap();
    let view = runtime.views(&generation).await.unwrap().remove(0);
    let response = runtime
        .handle(
            &generation,
            &view.target,
            PluginManagementRequest {
                request_context: None,
                headers: vec![],
                method: "GET".into(),
                path: "facts".into(),
                query: String::new(),
                content_type: None,
                body: vec![],
                request_id: "account-http-fixture".into(),
            },
        )
        .await
        .unwrap();
    let result = serde_json::from_slice(&response.body).unwrap();
    drop(generation);
    runtime.shutdown().await;
    super::wait_until_empty(cache.path()).await;
    result
}

async fn request_head(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 16 * 1024);
        bytes.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(bytes).unwrap()
}

struct ProxyFacts {
    resolutions: std::sync::Mutex<
        std::collections::VecDeque<Result<gateway_core::account::OutboundProxy, AdminError>>,
    >,
    page: Option<gateway_admin::model::proxies::ProxyPage>,
}

#[async_trait]
impl gateway_admin::ports::proxy::PluginProxyAccess for ProxyFacts {
    async fn resolve(&self, id: &str) -> Result<gateway_core::account::OutboundProxy, AdminError> {
        assert_eq!(id, "proxy_login");
        self.resolutions
            .lock()
            .unwrap()
            .pop_front()
            .expect("每次发送读取当前配置")
    }

    async fn list(
        &self,
        query: gateway_admin::model::proxies::ProxyListQuery,
    ) -> Result<gateway_admin::model::proxies::ProxyPage, AdminError> {
        assert_eq!(query.page, 2);
        assert_eq!(query.page_size.get(), 1);
        assert_eq!(query.search, "Login");
        Ok(self.page.clone().unwrap())
    }
}

#[tokio::test]
async fn selected_http_uses_account_or_proxy_without_fallback_or_redirects() {
    use gateway_core::account::OutboundProxy;
    use tokio::io::AsyncWriteExt as _;

    for (selector, scenarios) in [
        (
            "account",
            &[
                "plain",
                "explicit-auth",
                "rotated",
                "disabled",
                "proxy-failed",
                "missing",
            ][..],
        ),
        (
            "proxy",
            &[
                "plain",
                "explicit-auth",
                "disabled",
                "proxy-failed",
                "missing",
                "redirect",
            ][..],
        ),
    ] {
        for &scenario in scenarios {
            let target = wiremock::MockServer::start().await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
            let successful = matches!(scenario, "plain" | "explicit-auth" | "redirect");
            let target_url = target.uri();
            let proxy_task = if successful {
                Some(tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let request = request_head(&mut socket).await;
                    let response = if scenario == "redirect" {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {target_url}/redirect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                    } else {
                        "HTTP/1.1 201 Created\r\nSet-Cookie: step=one\r\nSet-Cookie: session=two\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                    };
                    socket.write_all(response.as_bytes()).await.unwrap();
                    request
                }))
            } else {
                drop(listener);
                None
            };
            let headers = if scenario == "explicit-auth" {
                vec![("authorization", b"Bearer plugin-fixture".to_vec())]
            } else {
                vec![]
            };
            let request =
                json!({"method":"GET", "url":format!("{}/login", target.uri()), "headers":headers});
            let results = if selector == "account" {
                let mut selected = account();
                selected.outbound_proxy = Some(proxy);
                selected.enabled = scenario != "disabled";
                if scenario == "rotated" {
                    selected.credential_revision = Revision::new(2).unwrap();
                }
                let accounts = Arc::new(Facts {
                    runtime_account: Some(selected),
                    ..Default::default()
                });
                management_queries(Some(accounts), None, json!([{
                    "method":"host.http.account", "params":{
                        "account_id":if scenario == "missing" {"acct_missing"} else {"acct_facts"},
                        "credential_revision":1, "request":request,
                    },
                }])).await
            } else {
                let resolution = match scenario {
                    "missing" => Err(AdminError::not_found("代理不存在")),
                    "disabled" => Err(AdminError::invalid("代理已禁用")),
                    _ => Ok(proxy),
                };
                let proxies = Arc::new(ProxyFacts {
                    resolutions: std::sync::Mutex::new([resolution].into()),
                    page: None,
                });
                management_queries(None, Some(proxies), json!([{
                    "method":"host.http.proxy", "params":{"proxy_id":"proxy_login", "request":request},
                }])).await
            };
            if let Some(task) = proxy_task {
                assert_eq!(
                    results[0]["status"],
                    if scenario == "redirect" { 302 } else { 201 }
                );
                let request = tokio::time::timeout(std::time::Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .to_ascii_lowercase();
                assert!(
                    request.starts_with("get http://127.0.0.1:")
                        && request.contains("/login http/1.1")
                );
                assert_eq!(
                    request
                        .lines()
                        .any(|line| line.starts_with("authorization:")),
                    scenario == "explicit-auth"
                );
                if scenario == "explicit-auth" {
                    assert!(request.contains("authorization: bearer plugin-fixture\r\n"));
                }
                if scenario != "redirect" {
                    assert_eq!(
                        results[0]["headers"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|header| header[0] == "set-cookie")
                            .count(),
                        2
                    );
                }
            } else {
                assert_eq!(
                    results[0]["error"],
                    match scenario {
                        "missing" => "rejected",
                        "rotated" => "conflict",
                        "disabled" if selector == "account" => "permission_denied",
                        "disabled" => "invalid_input",
                        _ => "upstream",
                    },
                    "{selector}/{scenario}: {results}"
                );
            }
            assert!(
                target.received_requests().await.unwrap().is_empty(),
                "{selector}/{scenario} must not connect directly"
            );
        }
    }
}

#[tokio::test]
async fn proxy_http_resolves_each_send_and_directory_only_exposes_selection_facts() {
    use gateway_admin::model::proxies::{ProxyPage, ProxyRecord};
    use gateway_core::account::OutboundProxy;
    use tokio::io::AsyncWriteExt as _;

    let target = wiremock::MockServer::start().await;
    let mut resolutions = std::collections::VecDeque::new();
    let mut tasks = Vec::new();
    for status in [201, 202] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        resolutions.push_back(Ok(OutboundProxy::parse(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        ))
        .unwrap()));
        tasks.push(tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request_head(&mut socket).await;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }));
    }
    let now = chrono::Utc::now();
    let proxies = Arc::new(ProxyFacts {
        resolutions: std::sync::Mutex::new(resolutions),
        page: Some(ProxyPage {
            total: 2,
            page: 2,
            page_size: 1,
            items: vec![ProxyRecord {
                id: "proxy_login".into(),
                name: "Login".into(),
                enabled: false,
                proxy: OutboundProxy::parse("http://user:secret@private.example:8080").unwrap(),
                revision: Revision::new(2).unwrap(),
                auto_location: false,
                detected_location: None,
                location: None,
                account_count: 0,
                last_test_at: None,
                last_test: None,
                created_at: now,
                updated_at: now,
            }],
        }),
    });
    let request = json!({"method":"host.http.proxy", "params":{
        "proxy_id":"proxy_login", "request":{"method":"GET", "url":target.uri(), "headers":[]},
    }});
    let results = management_queries(
        None,
        Some(proxies.clone()),
        json!([
            request, request,
            {"method":"host.proxies.list", "query":{"page":2,"page_size":1,"search":"Login"}},
        ]),
    )
    .await;
    assert_eq!(results[0]["status"], 201);
    assert_eq!(results[1]["status"], 202);
    assert_eq!(
        results[2],
        json!({"items":[{"id":"proxy_login","name":"Login","enabled":false}],"total":2,"page":2,"page_size":1})
    );
    assert!(proxies.resolutions.lock().unwrap().is_empty());
    assert!(target.received_requests().await.unwrap().is_empty());
    for task in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }
}
