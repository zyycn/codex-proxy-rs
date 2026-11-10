//! 验证模型目录、上游拒绝反馈与未知模型的账号选号边界

use super::*;

fn operation(model: &str) -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!(model)),
            ("input".to_owned(), json!("hello")),
        ]),
    )
    .unwrap()
    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
    Operation::Generate(GenerateRequest::from_protocol_payload(payload))
}

#[tokio::test]
async fn grok_models_never_acquire_chatgpt_oauth_accounts() {
    let oauth = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        Arc::default(),
        oauth.uri(),
        leases.clone(),
    );
    for model in ["grok-4.7", "xai/grok-4.7", "GROK-NEW", "grok"] {
        let error = provider
            .clone()
            .execute(
                planned_request_for_model("openai", operation(model), model),
                context("req_grok_oauth_rejected", CancellationToken::new()),
            )
            .await
            .err()
            .expect("Grok cannot select a ChatGPT account");
        assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    }
    assert!(leases.requests.lock().unwrap().is_empty());
    assert!(oauth.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn mixed_pool_routes_grok_to_api_key_despite_oauth_weight() {
    let api = MockServer::start().await;
    let oauth = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_scope_new").await;
    store.set_scheduling("acct_scope_new", None, AccountWeight::new(100).unwrap());
    store
        .seed_api_key(
            "acct_provider_contract",
            api.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    store.set_scheduling(
        "acct_provider_contract",
        None,
        AccountWeight::new(1).unwrap(),
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&api)
        .await;
    let mut stream = provider_with_base_url(&store, oauth.uri())
        .execute(
            planned_request_for_model("openai", operation("grok-4.7"), "grok-4.7"),
            context("req_grok_api_key", CancellationToken::new()),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_provider_contract"
    );
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    let requests = api.received_requests().await.unwrap();
    assert_eq!(captured_request_body(&requests[0])["model"], "grok-4.7");
    assert!(oauth.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unlisted_models_remain_available_without_catalog_discovery() {
    for api_key in [false, true] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        if api_key {
            store
                .seed_api_key(
                    "acct_provider_contract",
                    server.uri(),
                    provider_openai::credential::ResponsesTransport::Http,
                )
                .await;
        } else {
            create_account(&store, "acct_provider_contract").await;
        }
        Mock::given(method("POST"))
            .and(path(if api_key {
                "/responses"
            } else {
                "/codex/responses"
            }))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut stream = provider_with_base_url(&store, server.uri())
            .execute(
                planned_request_for_model(
                    "openai",
                    operation("custom-unlisted"),
                    "custom-unlisted",
                ),
                context("req_unlisted", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            captured_request_body(&requests[0])["model"],
            "custom-unlisted"
        );
    }
}

#[tokio::test]
async fn account_allowlists_override_catalog_omissions_but_weight_affinity_and_pinning_do_not() {
    use gateway_core::account::{
        AccountModelAccess,
        AccountModelAccessMode::{All, Allowlist, Denylist},
    };
    use provider_openai::credential::ResponsesTransport;

    for (primary_api, secondary_api) in [(false, false), (true, true), (true, false), (false, true)]
    {
        let native = MockServer::start().await;
        let api_primary = MockServer::start().await;
        let api_secondary = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        for (id, api, server, plan, models) in [
            (
                "acct_provider_contract",
                primary_api,
                &api_primary,
                "pro",
                vec!["gpt-6-luna", "gpt-6-astra"],
            ),
            (
                "acct_scope_new",
                secondary_api,
                &api_secondary,
                "pro",
                vec!["gpt-6-luna"],
            ),
        ] {
            if api {
                store
                    .seed_api_key(id, server.uri(), ResponsesTransport::Http)
                    .await;
                Mock::given(method("GET"))
                    .and(path("/models"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": models.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()
                    })))
                    .expect(1)
                    .mount(server)
                    .await;
            } else {
                let mut identity = profile(&format!("chatgpt-{id}"));
                identity.plan_type = Some(plan.to_owned());
                store
                    .seed_oauth_credential(ImportCodexOAuthCredential {
                        account_id: id.to_owned(),
                        name: plan.to_owned(),
                        secret: secret("test-access"),
                        verified_account: identity,
                        next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
                        enabled: true,
                    })
                    .await;
                Mock::given(method("GET"))
                    .and(path("/codex/models"))
                    .and(header("chatgpt-account-id", format!("chatgpt-{id}")))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "models": models.iter().map(|id| json!({"slug": id, "display_name": id})).collect::<Vec<_>>()
                    })))
                    .expect(1)
                    .mount(&native).await;
            }
        }
        for (server, endpoint) in [
            (&native, "/codex/responses"),
            (&api_primary, "/responses"),
            (&api_secondary, "/responses"),
        ] {
            Mock::given(method("POST"))
                .and(path(endpoint))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
                )
                .mount(server)
                .await;
        }
        store.set_scheduling("acct_scope_new", None, AccountWeight::new(100).unwrap());
        let affinity = Arc::new(MemorySessionAffinity::default());
        let leases = Arc::new(TestLeaseCoordinator::default());
        let (provider, _) = provider_and_quota_with_catalog(
            &store,
            affinity.clone(),
            native.uri(),
            leases.clone(),
            u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
        )
        .await;
        provider.query_model_capabilities().await.unwrap();
        for (model, expected) in [
            ("gpt-6-luna", "acct_scope_new"),
            ("gpt-6-astra", "acct_provider_contract"),
        ] {
            let request_operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(
                    "openai",
                    json!({
                        "model": model, "input": "hello",
                        "client_metadata": {"x-codex-turn-metadata": json!({
                            "session_id":"model-session", "thread_id":"model-session"
                        }).to_string()}
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                )
                .unwrap()
                .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
            ));
            let mut stream = provider
                .clone()
                .execute(
                    planned_request_for_model("openai", request_operation, model),
                    context("req_catalog_model", CancellationToken::new()),
                )
                .await
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                expected,
                "{model}: primary_api={primary_api}, secondary_api={secondary_api}"
            );
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        }
        assert_eq!(affinity.binding_count(), 1);
        let primary = "acct_provider_contract";
        let secondary = "acct_scope_new";
        // 目录外许可只属于明确添加该模型的账号，不能借用同池账号的白名单
        for (model, required, primary_mode, secondary_mode, expected) in [
            ("gpt-6-astra", Some(secondary), All, All, None),
            ("custom-unlisted", None, All, All, None),
            ("gpt-6.1-sol", None, Allowlist, All, Some(primary)),
            ("gpt-6-astra", None, Denylist, Allowlist, Some(secondary)),
            (
                "custom-unlisted",
                None,
                Denylist,
                Allowlist,
                Some(secondary),
            ),
            ("gpt-6-luna", Some(primary), Denylist, Allowlist, None),
        ] {
            let accounts = [(primary, primary_mode), (secondary, secondary_mode)]
                .into_iter()
                .map(|(id, mode)| {
                    let access = if mode == All {
                        AccountModelAccess::all()
                    } else {
                        AccountModelAccess::new(mode, vec![model.to_owned()]).unwrap()
                    };
                    (
                        ProviderAccountId::new(id).unwrap(),
                        RuntimeAccount::new(ProviderKind::new("openai").unwrap(), BTreeSet::new())
                            .with_model_access(access),
                    )
                })
                .collect();
            let scope = Arc::new(FrozenAccountScope::new(
                Arc::new(RuntimeAccountDirectory::new(accounts)),
                ClientRoutingScope::all_accounts(),
            ));
            let attempt = AttemptContext::new(
                RequestAttemptContext::new(
                    ModelRequestId::new("req_account_catalog_policy").unwrap(),
                    ClientApiKeyId::new("key_openai_contract").unwrap(),
                ),
                NonZeroU32::MIN,
                SystemTime::now() + Duration::from_secs(30),
                account_policy(),
                AccountAttemptContext::new(
                    BTreeSet::new(),
                    required.map(|id| ProviderAccountId::new(id).unwrap()),
                    None,
                )
                .with_account_scope(scope),
                None,
                CancellationToken::new(),
            );
            let acquisitions = leases.requests.lock().unwrap().len();
            let result = provider
                .clone()
                .execute(
                    planned_request_for_model("openai", operation(model), model),
                    attempt,
                )
                .await;
            if let Some(expected) = expected {
                let mut stream = result.unwrap();
                assert_eq!(stream.metadata().provider_account_id().as_str(), expected);
                while let Some(event) = stream.next().await {
                    event.unwrap();
                }
            } else {
                let error = result.err().expect("account must be excluded");
                assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
                assert_eq!(leases.requests.lock().unwrap().len(), acquisitions);
            }
        }
        let sent: Vec<_> = native
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .chain(api_primary.received_requests().await.unwrap())
            .chain(api_secondary.received_requests().await.unwrap())
            .filter(|request| request.method == "POST")
            .map(|request| {
                captured_request_body(&request)["model"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(sent.len(), 5);
        assert!(sent.iter().any(|model| model == "gpt-6.1-sol"));
        assert!(sent.iter().any(|model| model == "custom-unlisted"));
    }
}

#[tokio::test]
async fn only_a_current_account_catalog_can_exclude_a_model() {
    use gateway_core::account::{CredentialCasOutcome, CredentialCasUpdate, ProviderAccountUpdate};
    use provider_openai::credential::ResponsesTransport;

    for scenario in ["missing", "unknown_plan", "rotated", "expired"] {
        let native = MockServer::start().await;
        let api = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"slug": "gpt-6-luna", "display_name": "Luna"}]
            })))
            .mount(&native)
            .await;
        let endpoint = if scenario == "unknown_plan" {
            let mut identity = profile("chatgpt-acct_scope_new");
            identity.plan_type = None;
            store
                .seed_oauth_credential(ImportCodexOAuthCredential {
                    account_id: "acct_scope_new".to_owned(),
                    name: "unknown".to_owned(),
                    secret: secret("test-access"),
                    verified_account: identity,
                    next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
                    enabled: true,
                })
                .await;
            "/codex/responses"
        } else {
            store
                .seed_api_key("acct_scope_new", api.uri(), ResponsesTransport::Http)
                .await;
            let response = if matches!(scenario, "missing" | "expired") {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "gpt-6-luna"}]}))
            };
            Mock::given(method("GET"))
                .and(path("/models"))
                .respond_with(response)
                .mount(&api)
                .await;
            "/responses"
        };
        let upstream = if scenario == "unknown_plan" {
            &native
        } else {
            &api
        };
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
            )
            .expect(if scenario == "unknown_plan" { 0 } else { 1 })
            .mount(upstream)
            .await;
        let provider = if scenario == "expired" {
            use gateway_core::account::AccountCatalogObservation;
            let a = store.account("acct_scope_new").unwrap();
            store
                .save_account_catalog(
                    &AccountCatalogObservation {
                        account_id: a.id().clone(),
                        revision: a.revision(),
                        plan: a.plan_type().map(str::to_owned),
                        etag: None,
                        observed_at: SystemTime::now() - Duration::from_secs(3600),
                    },
                    b"\x01{\"data\":[{\"id\":\"gpt-6-luna\"}]}",
                )
                .await
                .unwrap();
            let (provider, _, _, catalog) = provider_and_quota_with_runtime_ports(
                &store,
                Arc::new(MemorySessionAffinity::default()),
                native.uri(),
                Arc::new(TestLeaseCoordinator::default()),
                u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
                Arc::new(MemoryCooldownPort::new()),
                crate::support::runtime_policy(),
            );
            catalog.restore().await.unwrap();
            // B 的失败不能清空上次成功结果，也不能继续以旧的缺失条目拒绝请求
            catalog.refresh_catalogs().await.unwrap();
            assert_eq!(
                catalog.cached_account_models(&a).unwrap().unwrap(),
                ["gpt-6-luna"]
            );
            provider
        } else {
            provider_with_catalog_and_base_url(&store, native.uri()).await
        };
        provider.query_model_capabilities().await.unwrap();
        if scenario == "rotated" {
            let account = store.account("acct_scope_new").unwrap();
            let loaded = store.load_current_credential(account.id()).await.unwrap();
            let update = CredentialCasUpdate::new(
                account.id().clone(),
                account.revision(),
                ProviderAccountUpdate {
                    account_id: account.id().clone(),
                    name: account.name().to_owned(),
                    email: None,
                    plan_type: None,
                },
                loaded.credential,
                false,
                None,
                None,
            )
            .unwrap();
            assert!(matches!(
                store.compare_and_swap_credential(update).await.unwrap(),
                CredentialCasOutcome::Updated(_)
            ));
        }
        let result = provider
            .execute(
                planned_request_for_model("openai", operation("gpt-6-astra"), "gpt-6-astra"),
                context("req_unknown_catalog", CancellationToken::new()),
            )
            .await;
        if scenario == "unknown_plan" {
            // 未知套餐也有该账号亲自返回的目录，不能借套餐缺失绕过资格
            assert_eq!(
                result.err().unwrap().kind(),
                ProviderErrorKind::NoEligibleAccount
            );
            continue;
        }
        let mut stream = result.unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_scope_new",
            "{scenario}"
        );
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
}

#[tokio::test]
async fn unsupported_reasoning_value_preserves_account_eligibility_across_transports() {
    for transport in ["http", "sse", "websocket"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let cooldowns = Arc::new(MemoryCooldownPort::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (provider, pool) = provider_with_cooldowns(
            &store,
            format!("http://{}", listener.local_addr().unwrap()),
            cooldowns.clone(),
        );
        let upstream_error = json!({
            "code": "unsupported_value",
            "type": "invalid_request_error",
            "param": "reasoning.effort",
            "message": "Unsupported value: 'none' is not supported with the 'gpt-6-astra' model. Supported values are: 'low', 'medium', 'high', 'xhigh', and 'max'."
        });
        let expected_message = upstream_error["message"].as_str().unwrap().to_owned();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            if transport == "websocket" {
                let mut socket = accept_codex_test_websocket(socket).await;
                socket.next().await.unwrap().unwrap();
                socket
                    .send(Message::Text(
                        json!({
                            "type": "error", "error": upstream_error, "status": 400
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            } else {
                read_http_request(&mut socket).await;
                let (status, content_type, body) = if transport == "sse" {
                    let event = json!({"type":"response.failed","response":{
                        "id":"resp_bad_effort","status":"failed","output":[],"error":upstream_error
                    }});
                    (
                        "200 OK",
                        "text/event-stream",
                        gateway_protocol::openai::sse::encode_sse_event(
                            "response.failed",
                            &event.to_string(),
                        ),
                    )
                } else {
                    (
                        "400 Bad Request",
                        "application/json",
                        json!({"error":upstream_error}).to_string(),
                    )
                };
                socket.write_all(format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ).as_bytes()).await.unwrap();
            }
            // 合法请求仍应使用同一个账号，不能被上一请求的参数拒绝隔离
            let (mut socket, _) = listener.accept().await.unwrap();
            read_http_request(&mut socket).await;
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{CAPTURE_COMPLETED_SSE}",
                CAPTURE_COMPLETED_SSE.len()
            ).as_bytes()).await.unwrap();
        });
        let payload = ProtocolPayload::json_object(
            "openai",
            json!({
                "model":"gpt-6-astra","input":"hello","reasoning":{"effort":"none"}
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap()
        .with_context(Map::from_iter([(
            "use_websocket".to_owned(),
            json!(transport == "websocket"),
        )]));
        let mut stream = provider
            .clone()
            .execute(
                planned_request_for_model(
                    "openai",
                    Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                    "gpt-6-astra",
                ),
                context("req_unsupported_effort", CancellationToken::new()),
            )
            .await
            .unwrap();
        let (_, error) = super::precommit::stream_failure(&mut stream).await;
        assert_eq!(
            error.kind(),
            ProviderErrorKind::InvalidRequest,
            "{transport}"
        );
        assert!(error.retry_is_prohibited(), "{transport}");
        assert!(!error.replay_is_safe(), "{transport}");
        let visible = error.client_visible_upstream_error().unwrap();
        assert_eq!(visible.code(), Some("unsupported_value"));
        assert_eq!(visible.message(), expected_message);
        drop(stream);
        assert!(cooldowns.scoped.lock().unwrap().is_empty(), "{transport}");
        let mut recovered = provider
            .clone()
            .execute(
                planned_request_for_model("openai", operation("gpt-6-astra"), "gpt-6-astra"),
                context("req_valid_effort", CancellationToken::new()),
            )
            .await
            .unwrap();
        assert_eq!(
            recovered.metadata().provider_account_id().as_str(),
            "acct_provider_contract"
        );
        while let Some(event) = recovered.next().await {
            event.unwrap();
        }
        server.await.unwrap();
        pool.shutdown().await;
    }
}

#[tokio::test]
async fn upstream_model_rejection_cools_only_that_model_across_requests_and_transports() {
    use gateway_core::provider_ports::{ProviderCooldownScope, ProviderScopedCooldown};

    for websocket in [false, true] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let cooldowns = Arc::new(MemoryCooldownPort::new());
        let (provider, pool) = provider_with_cooldowns(&store, server.uri(), cooldowns.clone());
        Mock::given(method(if websocket { "GET" } else { "POST" }))
            .and(path("/codex/responses"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":{
                "code":"model_not_found", "type":"invalid_request_error",
                "message":"The 'gpt-6-astra' model is not supported when using Codex with a ChatGPT account."
            }}))).mount(&server).await;
        let request_operation = if websocket {
            let payload = ProtocolPayload::json_object(
                "openai",
                Map::from_iter([
                    ("model".to_owned(), json!("gpt-6-astra")),
                    ("input".to_owned(), json!("hello")),
                ]),
            )
            .unwrap();
            Operation::Generate(GenerateRequest::from_protocol_payload(payload))
        } else {
            operation("gpt-6-astra")
        };
        let mut stream = provider
            .clone()
            .execute(
                planned_request_for_model("openai", request_operation, "gpt-6-astra"),
                context("req_model_rejected", CancellationToken::new()),
            )
            .await
            .unwrap();
        let error = loop {
            match stream.next().await {
                Some(Err(error)) => break error,
                Some(Ok(_)) => {}
                None => panic!("expected model rejection"),
            }
        };
        assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
        assert!(error.replay_is_safe());
        drop(stream);
        let blocked = provider
            .clone()
            .execute(
                planned_request_for_model("openai", operation("gpt-6-astra"), "gpt-6-astra"),
                context("req_model_cooled", CancellationToken::new()),
            )
            .await
            .err()
            .expect("model cooldown must survive the first request");
        assert_eq!(blocked.kind(), ProviderErrorKind::NoEligibleAccount);
        assert!(cooldowns.cooldowns.lock().unwrap().is_empty());
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
            )
            .expect(2)
            .mount(&server)
            .await;
        let mut other = provider
            .clone()
            .execute(
                planned_request_for_model("openai", operation("gpt-6-luna"), "gpt-6-luna"),
                context("req_other_model", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = other.next().await {
            event.unwrap();
        }
        let account = store.account("acct_provider_contract").unwrap();
        let scope =
            ProviderCooldownScope::upstream_model(UpstreamModelId::new("gpt-6-astra").unwrap());
        let key = (account.id().clone(), scope.clone());
        {
            let mut scoped = cooldowns.scoped.lock().unwrap();
            let until = scoped[&key].until();
            assert!(
                (Duration::from_secs(29 * 60)..=Duration::from_secs(30 * 60))
                    .contains(&until.duration_since(SystemTime::now()).unwrap())
            );
            scoped.insert(
                key,
                ProviderScopedCooldown::new(
                    account.id().clone(),
                    account.revision(),
                    scope,
                    SystemTime::now() - Duration::from_secs(1),
                ),
            );
        }
        let mut recovered = provider
            .clone()
            .execute(
                planned_request_for_model("openai", operation("gpt-6-astra"), "gpt-6-astra"),
                context("req_model_recovered", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = recovered.next().await {
            event.unwrap();
        }
        pool.shutdown().await;
    }
}
