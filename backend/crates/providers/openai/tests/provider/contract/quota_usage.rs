//! 失败流的额度观测不能冒充使用，真实输出仍应阻止本轮预热

use super::*;

#[tokio::test]
async fn stream_quota_facts_skip_warmup_only_after_usage() {
    for ending in ["failed", "error", "eof", "malformed", "completed"] {
        for output_seen in [false, true] {
            let store = Arc::new(MemoryAccountStore::default());
            let account_id = "acct_provider_contract";
            create_account(&store, account_id).await;
            store.set_quota_settings(
                account_id,
                serde_json::from_value(json!({
                    "codex:18000s": {"warmup": {"model": "gpt-5.4"}}
                }))
                .unwrap(),
            );
            let now = Utc::now();
            let reset = now.timestamp() - 600;
            let observed_at = SystemTime::from(now - chrono::Duration::seconds(660));
            let account = store.account(account_id).unwrap();
            let usage = |reset| {
                json!({"rate_limit": {"allowed": true, "primary_window": {
                    "used_percent": 0, "limit_window_seconds": 18000, "reset_at": reset
                }}})
            };
            store
                .compare_and_swap_quota(QuotaObservation {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    quota: OpaqueProviderData::new(usage(reset).as_object().unwrap().clone()),
                    observed_at,
                    state: QuotaState::allowed(observed_at),
                    quota_pause: None,
                    expected_connection: None,
                    plan_type: None,
                })
                .await
                .unwrap();
            let server = MockServer::start().await;
            let (provider, quota) = provider_and_quota_with_affinity_and_base_url_and_leases(
                &store,
                Arc::new(MemorySessionAffinity::default()),
                server.uri(),
                Arc::new(TestLeaseCoordinator::default()),
                0,
            );
            quota
                .execute_bucket_warmup_at(now - chrono::Duration::seconds(601))
                .await
                .unwrap();
            let rate_limit = json!({"type": "codex.rate_limits", "rate_limits": {
                "allowed": true, "primary": {"used_percent": 0, "window_minutes": 300,
                "reset_at": reset + 18000}
            }});
            let mut body = sse_event(json!({"type": "response.created", "response": {
                "id": "resp_quota_usage", "model": "gpt-5.4"
            }}));
            body.push_str(&sse_event(rate_limit));
            if output_seen {
                body.push_str(&sse_event(json!({"type": "response.output_text.delta",
                    "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "hello"
                })));
            }
            body.push_str(&match ending {
                "failed" => sse_event(json!({"type": "response.failed", "status_code": 400,
                    "response": {"id": "resp_quota_usage", "status": "failed", "error": {
                        "code": "invalid_request_error", "message": "fixture rejection"
                    }}
                })),
                "error" => sse_event(json!({"type": "error", "status": 400, "error": {
                    "type": "invalid_request_error", "code": "invalid_request_error",
                    "message": "fixture rejection"
                }})),
                "malformed" => "event: response.completed\ndata: {invalid json}\n\n".to_owned(),
                "completed" => completed_response(),
                _ => String::new(),
            });
            Mock::given(method("POST"))
                .and(path("/codex/responses"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .insert_header("x-codex-primary-used-percent", "0")
                        .insert_header("x-codex-primary-window-minutes", "300")
                        .insert_header("x-codex-primary-reset-at", (reset + 18000).to_string())
                        .set_body_string(body),
                )
                .up_to_n_times(1)
                .with_priority(1)
                .expect(1)
                .mount(&server)
                .await;
            let mut stream = provider
                .execute(
                    planned_request("openai", http_generate_operation()),
                    context("req_quota_usage", CancellationToken::new()),
                )
                .await
                .unwrap();
            while let Some(event) = stream.next().await {
                if let Err(error) = event {
                    assert_ne!(ending, "completed", "successful fixture failed: {error}");
                    break;
                }
            }
            drop(stream);
            let saved = store.quota_json(account_id).unwrap();
            assert_eq!(
                saved.pointer("/rate_limits_by_limit_id/codex/primary_window/used_percent"),
                Some(&json!(0.0))
            );
            let used_quota = output_seen || ending == "completed";
            assert_eq!(
                saved.pointer("/_cpr_bucket_last_used_at/codex").is_some(),
                used_quota,
                "{ending}"
            );

            let expected_warmups = u64::from(!used_quota);
            Mock::given(method("GET"))
                .and(path("/api/codex/usage"))
                .respond_with(ResponseTemplate::new(200).set_body_json(usage(reset + 18000)))
                .expect(expected_warmups)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/codex/responses"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(completed_response()),
                )
                .expect(expected_warmups)
                .mount(&server)
                .await;
            assert_eq!(
                quota
                    .execute_bucket_warmup_at(Utc::now())
                    .await
                    .unwrap()
                    .warmed_up,
                expected_warmups,
                "{ending}, output={output_seen}"
            );
            server.verify().await;
        }
    }
}

fn sse_event(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

fn completed_response() -> String {
    sse_event(json!({"type": "response.completed", "response": {
        "id": "resp_quota_usage", "model": "gpt-5.4", "status": "completed", "output": [],
        "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
    }}))
}

#[tokio::test]
async fn disconnected_stream_preserves_quota_facts_without_marking_usage() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_provider_contract";
    create_account(&store, account_id).await;
    let body = sse_event(json!({"type": "codex.rate_limits", "rate_limits": {
        "primary": {"used_percent": 0, "window_minutes": 300,
            "reset_at": Utc::now().timestamp() + 18000}
    }})) + &sse_event(json!({"type": "response.created", "response": {
        "id": "resp_disconnect", "model": "gpt-5.4"
    }}));
    let (base_url, _release, _first_chunk_sent, server) =
        paused_chunked_sse_server(body, String::new()).await;
    let (provider, _) = provider_and_quota_with_affinity_and_base_url_and_leases(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        base_url,
        Arc::new(TestLeaseCoordinator::default()),
        0,
    );
    let mut stream = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_quota_disconnect", CancellationToken::new()),
        )
        .await
        .unwrap();
    stream.next().await.unwrap().unwrap();
    server.abort();
    let mut failed = false;
    while let Some(event) = stream.next().await {
        if event.is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed, "the fixture must fail while reading the stream");
    let saved = store.quota_json(account_id).unwrap();
    assert_eq!(
        saved.pointer("/rate_limits_by_limit_id/codex/primary_window/used_percent"),
        Some(&json!(0.0))
    );
    assert!(saved.get("_cpr_bucket_last_used_at").is_none());
}
