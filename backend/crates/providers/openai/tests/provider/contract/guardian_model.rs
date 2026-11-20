//! 同步审核的结构化身份、热更新路由与上游续接合同

use super::*;

const LUNA: &str = "gpt-5.6-luna";
const REVIEW: &str = gateway_protocol::openai::CODEX_AUTO_REVIEW_MODEL;
const FUTURE: &str = "custom-review-model";

fn reviewer_operation(model: &str, source: &str, subagent: &str, input: Value) -> Operation {
    let body = json!({
        "model": model, "input": input,
        "client_metadata": {
            "x-openai-subagent": subagent,
            "x-codex-turn-metadata": json!({"thread_source": source}).to_string()
        }
    });
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
    ))
}

fn settings(enabled: bool) -> SettingsValues {
    SettingsValues::new(2, "smart", Default::default(), None, None)
        .with_openai_oauth_guardian_review_model(enabled)
}

fn selected_session(model: &str) -> ProviderSessionState {
    ProviderSessionState::new(
        "openai",
        json!({"account_id":"acct_provider_contract","continuation_scope":"persisted","reviewer_model":model})
            .as_object().unwrap().clone(),
    ).unwrap()
}

fn reviewer_snapshot() -> RuntimeSnapshot {
    let provider = ProviderKind::new("openai").unwrap();
    RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        settings(false),
        vec![provider.clone()],
        [FUTURE, REVIEW]
            .map(|model| {
                ProviderModel::new(
                    provider.clone(),
                    UpstreamModelId::new(model).unwrap(),
                    ModelCapabilities::new([OperationKind::Generate].into(), Some(32_000))
                        .with_upstream_feature_validation(),
                )
            })
            .into(),
        vec![],
    )
    .unwrap()
}

fn plan_reviewer(
    snapshot: &RuntimeSnapshot,
    provider: &CodexProvider,
    operation: Operation,
    previous: Option<&ProviderSessionState>,
) -> ProviderRequest {
    let requested = PublicModelId::new(FUTURE).unwrap();
    let openai = ProviderKind::new("openai").unwrap();
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            [(
                ProviderAccountId::new("acct_provider_contract").unwrap(),
                RuntimeAccount::new(openai.clone(), BTreeSet::new())
                    .with_direct_authentication_kind(Some("oauth".into())),
            )]
            .into(),
        )),
        ClientRoutingScope::all_accounts(),
    ));
    let context = RoutingContext::default();
    let plan = match provider.direct_account_model(
        &requested,
        &operation,
        previous,
        snapshot.settings(),
    ) {
        Some(target) => snapshot.plan_with_direct_account_model(
            &requested,
            &operation,
            scope,
            &context,
            gateway_core::routing::DirectAccountModel {
                provider: &openai,
                model: &target,
                continuation_owner: None,
            },
            |_| true,
        ),
        None => snapshot.plan(&requested, &operation, scope, &context),
    }
    .unwrap();
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

#[tokio::test]
async fn guardian_model_matches_structured_reviewer_and_any_original_model() {
    let store = Arc::new(MemoryAccountStore::default());
    let provider = provider(&store);
    for requested in [LUNA, FUTURE] {
        let model = PublicModelId::new(requested).unwrap();
        for input in [json!([]), json!([{"role": "user", "content": requested}])] {
            let op = reviewer_operation(requested, "guardian_review", "guardian", input);
            assert!(
                provider
                    .direct_account_model(&model, &op, None, &settings(false))
                    .is_none()
            );
            assert_eq!(
                provider
                    .direct_account_model(&model, &op, None, &settings(true))
                    .unwrap()
                    .as_str(),
                REVIEW
            );
        }
    }
    for (raw_model, routed_model, source, role) in [
        (FUTURE, FUTURE, "guardian_classifier", "guardian"),
        (FUTURE, FUTURE, "guardian_housekeeper", "guardian"),
        (FUTURE, FUTURE, "guardian_review", "review"),
        (FUTURE, FUTURE, "cli", "guardian"),
        (REVIEW, REVIEW, "guardian_review", "guardian"),
        (FUTURE, "admin-routed", "guardian_review", "guardian"),
    ] {
        let op = reviewer_operation(raw_model, source, role, json!([]));
        assert!(
            provider
                .direct_account_model(
                    &PublicModelId::new(routed_model).unwrap(),
                    &op,
                    None,
                    &settings(true),
                )
                .is_none()
        );
    }
}

#[tokio::test]
async fn guardian_model_rejects_unknown_or_changed_continuations_before_account_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    let provider = provider(&store);
    let snapshot = reviewer_snapshot();
    for (enabled, previous) in [(true, None), (true, Some(FUTURE)), (false, Some(REVIEW))] {
        let op = continuing_reviewer(previous);
        let request = plan_reviewer(
            &snapshot.with_settings(&settings(enabled)).unwrap(),
            &provider,
            op,
            None,
        );
        let result = Arc::clone(&provider)
            .execute(request, external_continuation_context("req_guardian_model"))
            .await;
        let error = match result {
            Err(err) => err,
            Ok(_) => panic!("unexpected call"),
        };
        assert_eq!(
            error.continuation_unavailable_reason(),
            Some("review_model_changed")
        );
    }
}

fn continuing_reviewer(previous: Option<&str>) -> Operation {
    let Operation::Generate(base) =
        reviewer_operation(FUTURE, "guardian_review", "guardian", json!([]))
    else {
        unreachable!()
    };
    let payload = base.protocol_payload();
    let mut body = payload.body().clone();
    body.insert(
        "previous_response_id".into(),
        json!("external-previous-response"),
    );
    let mut request = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body)
            .unwrap()
            .with_context(payload.context().clone()),
    );
    if let Some(previous) = previous {
        request = request.with_provider_session_state(selected_session(previous));
    }
    Operation::Generate(request)
}

#[tokio::test]
async fn guardian_samplers_keep_the_http_transport_chosen_by_the_client() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(2)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    for turn in [
        json!({"thread_source":"guardian_classifier", "turn_trigger":"guardian_classifier"}),
        json!({"turn_trigger":"guardian_housekeeper"}),
    ] {
        let body = json!({"model":LUNA, "input":[], "stream":true, "store":false,
            "client_metadata":{"x-openai-subagent":"guardian", "x-codex-turn-metadata":turn.to_string()}});
        let payload = ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([
                ("subagent".into(), json!("guardian")),
                ("turn_metadata".into(), json!(turn.to_string())),
            ]));
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request("openai", operation),
                context("req_sampler_http", CancellationToken::new()),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "both sampler POSTs should bypass a fresh WS opening"
    );
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("content-encoding"))
    );
}

#[tokio::test]
async fn explicit_guardian_headers_only_select_a_unique_role() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let conflicting = ["reviewer", "classifier"];
    let reversed = ["classifier", "reviewer"];
    let cases: &[(&[&str], &[&str], bool)] = &[
        (&conflicting, &[], true),
        (&reversed, &[], true),
        (&["classifier", "classifier"], &[], false),
        (&conflicting, &["classifier"], false),
        (&["classifier"], &reversed, true),
    ];
    for &(client, middleware, ambiguous) in cases {
        let headers: Vec<_> = client
            .iter()
            .map(|v| json!(["x-codex-guardian", STANDARD.encode(v)]))
            .collect();
        let attempt = if middleware.is_empty() {
            context("req_guardian_headers", CancellationToken::new())
        } else {
            context_with_middleware(
                "req_guardian_headers",
                Arc::new(RecordingMiddleware {
                    observed: Default::default(),
                    replacement: ("input".into(), json!([])),
                    request_headers: middleware
                        .iter()
                        .map(|v| {
                            MiddlewareHeader::new(
                                "x-codex-guardian",
                                Bytes::copy_from_slice(v.as_bytes()),
                            )
                        })
                        .collect(),
                }),
                FastMode::Default,
            )
        };
        let values = if middleware.is_empty() {
            client
        } else {
            middleware
        };
        let body = json!({"model": LUNA, "input": [], "service_tier":"default"});
        let payload = ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_context(Map::from_iter([
                ("use_websocket".into(), json!(false)),
                ("opaque_request_headers".into(), json!(headers)),
            ]));
        let op = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
        let mut stream = provider
            .clone()
            .execute(
                planned_request_with_presentation("openai", op, LUNA, None),
                attempt,
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let sent = server.received_requests().await.unwrap().pop().unwrap();
        assert_eq!(
            sent.headers
                .get_all("x-codex-guardian")
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect::<Vec<_>>(),
            values
        );
        assert_eq!(sent.headers.contains_key("content-encoding"), ambiguous);
        assert_eq!(sent.headers.contains_key("x-codex-routing-hint"), ambiguous);
        assert_eq!(
            captured_request_body(&sent).get("service_tier").is_some(),
            ambiguous
        );
    }
}

#[tokio::test]
async fn guardian_model_freezes_the_selected_model_across_settings_changes_and_continuations() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(4)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let off = reviewer_snapshot();
    let on = off.with_settings(&settings(true)).unwrap();
    for (snapshot, previous, expected) in [
        (&off, None, FUTURE),
        (&on, None, REVIEW),
        (&off, Some(REVIEW), REVIEW),
        (&on, Some(FUTURE), FUTURE),
    ] {
        let operation = match previous {
            Some(model) => continuing_reviewer(Some(model)),
            None => reviewer_operation(
                FUTURE,
                "guardian_review",
                "guardian",
                json!([{"role":"user","content":FUTURE}]),
            ),
        };
        let request = plan_reviewer(
            snapshot,
            &provider,
            operation,
            previous.map(selected_session).as_ref(),
        );
        let ctx = if previous.is_some() {
            external_continuation_context("req_guardian_wire")
        } else {
            context("req_guardian_wire", CancellationToken::new())
        };
        let mut stream = Arc::clone(&provider).execute(request, ctx).await.unwrap();
        assert_eq!(
            stream.metadata().upstream_model().unwrap().as_str(),
            expected
        );
        let mut session = None;
        while let Some(event) = stream.next().await {
            if let Some(state) = event.unwrap().session_update() {
                session = state.payload().get("reviewer_model").cloned();
            }
        }
        assert_eq!(session, Some(json!(expected)));
        let sent = captured_request_body(server.received_requests().await.unwrap().last().unwrap());
        assert_eq!(sent["model"], expected);
        if previous.is_some() {
            assert_eq!(sent["previous_response_id"], "external-previous-response");
        } else {
            assert_eq!(sent["input"][0]["content"], FUTURE);
        }
    }
}
