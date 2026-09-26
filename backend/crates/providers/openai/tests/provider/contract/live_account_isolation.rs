//! 显式指定本机导出文件后才运行；凭据只放入内存，不刷新、不落盘、不打印响应或身份。

use super::*;
use gateway_core::engine::response_control::ResponseControl;
use provider_openai::credential::CodexOAuthSecret;
use secrecy::SecretString;

fn live_context(
    account: &str,
    owner: Option<&str>,
    control: Option<ResponseControl>,
    continuation: Option<ContinuationBinding>,
) -> AttemptContext {
    let trace = control.as_ref().map_or_else(Default::default, |_| {
        gateway_core::diagnostics::TraceContext::new("req_live_isolation")
    });
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_live_isolation").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_response_control(control)
        .with_trace(trace),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(90),
        account_policy(),
        AccountAttemptContext::diagnostic(
            BTreeSet::new(),
            ProviderAccountId::new(account).unwrap(),
            owner.map(|owner| {
                ProviderAccountStateOwner::new(
                    ProviderKind::new("openai").unwrap(),
                    ProviderAccountId::new(owner).unwrap(),
                )
            }),
        ),
        continuation,
        CancellationToken::new(),
    )
}

async fn live_store() -> Arc<MemoryAccountStore> {
    let path =
        std::env::var("CPR_LIVE_ACCOUNTS_FILE").expect("set CPR_LIVE_ACCOUNTS_FILE explicitly");
    let bytes = std::fs::read(path).expect("read local account export");
    let document: Value = serde_json::from_slice(&bytes).expect("parse local account export");
    let accounts: Vec<&Value> = document["documents"]
        .as_array()
        .expect("documents")
        .iter()
        .filter(|doc| doc["provider"] == "openai")
        .flat_map(|doc| doc["document"]["accounts"].as_array().expect("accounts"))
        .take(2)
        .collect();
    assert!(accounts.len() == 2, "two OpenAI accounts required");
    let store = Arc::new(MemoryAccountStore::default());
    for (account, id) in accounts
        .into_iter()
        .zip(["acct_scope_old", "acct_scope_new"])
    {
        let text = |key: &str| {
            account[key]
                .as_str()
                .expect("required account field")
                .to_owned()
        };
        let expiration = chrono::DateTime::parse_from_rfc3339(&text("accessTokenExpiresAt"))
            .expect("expiry timestamp")
            .with_timezone(&Utc);
        assert!(
            expiration > Utc::now(),
            "live access token expired; test does not rotate credentials"
        );
        let mut verified = profile(&text("accountId"));
        verified.chatgpt_user_id = text("userId");
        verified.oauth_subject = verified.chatgpt_user_id.clone();
        verified.poid = None;
        verified.email = None;
        verified.access_token_expires_at = Some(expiration);
        store
            .seed_oauth_credential(ImportCodexOAuthCredential {
                account_id: id.to_owned(),
                name: id.to_owned(),
                secret: CodexOAuthSecret {
                    access_token: SecretString::from(text("accessToken")),
                    refresh_token: None,
                    id_token: None,
                },
                verified_account: verified,
                next_refresh_at: None,
                enabled: true,
            })
            .await;
    }
    store
}

async fn live_response(
    provider: Arc<CodexProvider>,
    websocket: bool,
    account: &str,
    owner: Option<&str>,
    input: Value,
    parent: Option<&str>,
) -> (String, Value) {
    eprintln!(
        "live request: transport={}, account={}, parent_reference={}",
        if websocket { "websocket" } else { "http" },
        account,
        parent.is_some()
    );
    let model = std::env::var("CPR_LIVE_MODEL")
        .expect("set CPR_LIVE_MODEL to a model available to both accounts");
    let mut body =
        json!({"model":model,"instructions":"Reply briefly.","input":input,"store":false});
    if let Some(parent) = parent {
        body["client_metadata"] = json!({"parent_response_id":parent});
    }
    let payload = ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
        .unwrap()
        .with_context(Map::from_iter([(
            "use_websocket".to_owned(),
            json!(websocket),
        )]));
    let mut stream = provider
        .execute(
            planned_request_for_model(
                "openai",
                Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
                &model,
            ),
            live_context(account, owner, None, None),
        )
        .await
        .unwrap_or_else(|error| {
            panic!(
                "live prepare failed: kind={:?}, status={:?}",
                error.kind(),
                error.upstream_status()
            )
        });
    let mut completed = None;
    let mut actual_transport = None;
    timeout(Duration::from_secs(90), async {
        while let Some(event) = stream.next().await {
            let event = event.unwrap_or_else(|error| {
                panic!(
                    "live stream failed: kind={:?}, status={:?}, parent_error={}, model_error={}",
                    error.kind(),
                    error.upstream_status(),
                    error
                        .client_visible_upstream_error()
                        .is_some_and(|detail| detail.message().contains("parent")
                            || detail.message().contains("Response with id")),
                    error
                        .client_visible_upstream_error()
                        .is_some_and(|detail| detail.message().contains("model")),
                )
            });
            if let Some(observation) = event.response_observation() {
                actual_transport = Some(observation.transport().as_str().to_owned());
            }
            if let Some(wire) = event.wire_event()
                && wire.event_type() == Some("response.completed")
            {
                let response = &wire.data()["response"];
                completed = Some((
                    response["id"].as_str().expect("response ID").to_owned(),
                    response["output"].clone(),
                ));
            }
        }
    })
    .await
    .expect("live response deadline");
    assert_eq!(
        actual_transport.as_deref(),
        Some(if websocket { "websocket" } else { "http_sse" })
    );
    completed.expect("live request reached response.completed")
}

#[tokio::test]
#[ignore = "requires a real account with WebSocket interrupt support and consumes inference quota"]
async fn live_websocket_interrupt_and_native_continuation() {
    let store = live_store().await;
    let model = std::env::var("CPR_LIVE_MODEL").expect("set CPR_LIVE_MODEL explicitly");
    let provider =
        provider_with_base_url_and_retry_budget(&store, OFFICIAL_CODEX_BASE_URL.to_owned(), 0);
    let account = "acct_scope_old";
    let session_id = uuid::Uuid::new_v4().to_string();
    let thread_id = uuid::Uuid::new_v4().to_string();
    let mut state = Some(
        ProviderSessionState::new(
            "openai",
            Map::from_iter([
                ("account_id".to_owned(), json!(account)),
                (
                    "conversation_id".to_owned(),
                    json!(uuid::Uuid::new_v4().to_string()),
                ),
                ("continuation_scope".to_owned(), json!("persisted")),
            ]),
        )
        .unwrap(),
    );
    let mut previous: Option<String> = None;
    for interrupt in [true, false] {
        let control = ResponseControl::default();
        let input = if interrupt {
            // 输出留出一个网络往返窗口，避免快速完成先于中断到达。
            "Write 200 numbered lines, with one short word on each line."
        } else {
            "Reply with exactly OK."
        };
        let payload = ProtocolPayload::json_object("openai", json!({
            "model":model,"instructions":"Follow the user's request.","input":[{"role":"user","content":input}],
            "store":false,
        }).as_object().unwrap().clone()).unwrap().with_context(Map::from_iter([
            ("use_websocket".to_owned(), json!(true)),
            ("session_id".to_owned(), json!(session_id)),
            ("thread_id".to_owned(), json!(thread_id)),
        ]));
        let request = GenerateRequest::from_protocol_payload(payload)
            .with_provider_session_state(state.take().expect("session checkpoint"));
        let continuation = previous.as_ref().map(|id| {
            ContinuationBinding::Pinned(NativeContinuationPin::new(
                PreviousResponseId::new(id),
                PreviousResponseId::new(id),
                ClientApiKeyId::new("key_openai_contract").unwrap(),
                ProviderKind::new("openai").unwrap(),
                ProviderAccountId::new(account).unwrap(),
            ))
        });
        let context = live_context(account, Some(account), Some(control.clone()), continuation);
        let trace = context.trace().clone();
        assert_eq!(trace.snapshot().unwrap()["wireDumpEnabled"], false);
        let mut stream = provider
            .clone()
            .execute(
                planned_request_for_model("openai", Operation::Generate(request), &model),
                context,
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "live interrupt prepare failed: {:?} {:?}",
                    error.kind(),
                    error.upstream_status()
                )
            });
        let mut sent = false;
        let mut terminal = false;
        let mut interruption_acknowledged = false;
        let mut output_bytes = 0;
        let mut actual_transport = None;
        let started_at = std::time::Instant::now();
        let result = timeout(Duration::from_secs(90), async {
            while let Some(event) = stream.next().await {
                let event = event.unwrap_or_else(|error| {
                    panic!(
                        "live interrupt stream failed: {:?} {:?}, after_interrupt={}, mentions_session={}, mentions_interrupt={}",
                        error.kind(),
                        error.upstream_status(),
                        sent,
                        error.client_visible_upstream_error().is_some_and(|detail| detail.message().to_ascii_lowercase().contains("session")),
                        error.client_visible_upstream_error().is_some_and(|detail| detail.message().to_ascii_lowercase().contains("interrupt")),
                    )
                });
                if let Some(update) = event.session_update() {
                    state = Some(update.clone());
                }
                if let Some(observation) = event.response_observation() {
                    let transport = observation.transport().as_str();
                    if actual_transport.as_deref() != Some(transport) {
                        eprintln!("live actual transport={transport}");
                        actual_transport = Some(transport.to_owned());
                    }
                }
                if let Some(wire) = event.wire_event() {
                    match wire.event_type() {
                        Some("response.created") if interrupt => {
                            let id = wire
                                .data()
                                .pointer("/response/id")
                                .and_then(Value::as_str)
                                .expect("response ID");
                            sent = control.interrupt(id).is_ok();
                            eprintln!("live interrupt accepted={}, elapsed_ms={}", sent, started_at.elapsed().as_millis());
                            assert!(sent, "current upstream response must support interruption; actual transport={actual_transport:?}");
                        }
                        Some("response.output_text.delta") => {
                            output_bytes += wire.data()["delta"].as_str().map_or(0, str::len);
                        }
                        Some("response.completed" | "response.incomplete") => {
                            let response = &wire.data()["response"];
                            eprintln!("live terminal type={:?}, elapsed_ms={}, output_bytes={}", wire.event_type(), started_at.elapsed().as_millis(), output_bytes);
                            if interrupt {
                                interruption_acknowledged = wire.event_type() == Some("response.incomplete")
                                    && response
                                        .pointer("/incomplete_details/reason")
                                        .and_then(Value::as_str)
                                        == Some("interrupted");
                            } else {
                                assert_eq!(wire.event_type(), Some("response.completed"));
                            }
                            previous =
                                Some(response["id"].as_str().expect("response ID").to_owned());
                            terminal = true;
                        }
                        _ => {}
                    }
                }
            }
        })
        .await;
        let interrupt_written = trace.snapshot().is_some_and(|snapshot| {
            snapshot["events"].as_array().is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event["stage"] == "upstream.interrupt.sent")
            })
        });
        eprintln!("live interrupt written={interrupt_written}, output_bytes={output_bytes}");
        result.expect("live interrupt response deadline");
        if interrupt {
            assert!(
                interrupt_written,
                "interrupt was written to the active socket"
            );
            assert!(
                interruption_acknowledged,
                "upstream acknowledged interruption"
            );
        }
        assert!(terminal && (!interrupt || sent));
        assert!(state.is_some());
        eprintln!(
            "live websocket {}: passed",
            if interrupt {
                "interrupt"
            } else {
                "native continuation"
            }
        );
    }
}

#[tokio::test]
#[ignore = "requires two real accounts via CPR_LIVE_ACCOUNTS_FILE and consumes minimal inference quota"]
async fn live_same_account_parent_and_cross_account_full_replay() {
    let store = live_store().await;
    let provider =
        provider_with_base_url_and_retry_budget(&store, OFFICIAL_CODEX_BASE_URL.to_owned(), 0);
    for websocket in [false, true] {
        let first = json!({"role":"user","content":"Reply with exactly OK."});
        let (parent, output) = live_response(
            provider.clone(),
            websocket,
            "acct_scope_old",
            None,
            json!([first]),
            None,
        )
        .await;
        let mut history = vec![first];
        history.extend(output.as_array().expect("output items").iter().cloned());
        history.push(json!({"role":"user","content":"Reply with exactly OK again."}));
        for account in ["acct_scope_old", "acct_scope_new"] {
            live_response(
                provider.clone(),
                websocket,
                account,
                Some("acct_scope_old"),
                json!(history),
                Some(&parent),
            )
            .await;
        }
        eprintln!(
            "live transport={} initial/same-account-parent/cross-account-full-replay: passed",
            if websocket { "websocket" } else { "http" }
        );
    }
}
