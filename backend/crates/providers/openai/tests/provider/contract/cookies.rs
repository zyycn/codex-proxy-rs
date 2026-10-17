//! 验证真实响应头只写回一次，流内失败不重复处理 Cookie

use super::*;

fn cookie_provider(store: &Arc<MemoryAccountStore>, base_url: String) -> Arc<CodexProvider> {
    let profile = wire_profile();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let leases = Arc::new(TestLeaseCoordinator::default());
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        catalog_cache(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        Arc::new(MemoryCooldownPort::new()),
        Arc::clone(&leases) as Arc<dyn ProviderLeasePort>,
        crate::support::runtime_policy(),
    ));
    let feedback = Arc::new(AccountFeedbackStats::default());
    let selector = Arc::new(CodexCredentialSelector::new(
        ProviderKind::new("openai").unwrap(),
        store.repository(),
        Arc::new(crate::support::MemoryClientInstallations::default()),
        leases,
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(MemorySessionExclusions::default()),
        Arc::clone(&quota),
        Arc::clone(&feedback),
        // 测试仅允许本机模拟上游，生产 Cookie 白名单不放宽
        CodexCookiePolicy::new(["cf_clearance"], ["127.0.0.1"]).unwrap(),
    ));
    Arc::new(
        CodexProvider::new(
            selector,
            catalog,
            quota,
            feedback,
            http,
            profile,
            base_url,
            Arc::new(CodexWebSocketPool::default()),
            0,
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn response_cookies_are_saved_once_for_stream_and_opening_failures() {
    for streaming in [true, false] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let original = store.account("acct_provider_contract").unwrap();
        let server = MockServer::start().await;
        let failure = json!({"type":"error", "error":{"type":"server_error", "code":"server_error", "message":"backend unavailable"}});
        let response = if streaming {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("event: error\ndata: {failure}\n\n"))
        } else {
            ResponseTemplate::new(503).set_body_json(&failure)
        }
        .insert_header("set-cookie", "cf_clearance=current; Path=/; Max-Age=3600");
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let mut stream = cookie_provider(&store, server.uri())
            .execute(
                planned_request("openai", http_generate_operation()),
                context("req_cookie_failure", CancellationToken::new()),
            )
            .await
            .unwrap();
        let mut failure = None;
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                failure = Some(error);
                break;
            }
        }
        assert!(failure.is_some(), "simulated upstream must fail");
        let current = store.account("acct_provider_contract").unwrap();
        assert_eq!(
            current.revision().get(),
            original.revision().get() + 1,
            "streaming={streaming}"
        );
        let data = store
            .repository()
            .load_complete_data(&current)
            .await
            .unwrap();
        assert_eq!(data.cookies()[0].value, "current");
        server.verify().await;
    }
}
