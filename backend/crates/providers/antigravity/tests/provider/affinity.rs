//! 验证三种入站协议的会话绑定、租户隔离与发送前原子认领

use crate::support::{Fixture, context_with_policy, context_with_scope, request};
use futures::TryStreamExt;
use gateway_core::{
    account::{
        AccountAffinity, AccountSelectionPolicy, ProviderAccountId, ProviderAccountStore,
        RotationStrategy,
    },
    concurrency::ConcurrencyQueuePolicy,
    engine::{
        AccountAttemptContext, AttemptContext, ModelRequestId, ProviderAccountStateOwner,
        RequestAttemptContext,
        provider::{Provider, ProviderRequest, ProviderStream},
    },
    lifecycle::CancellationToken,
    operation::{GenerateRequest, Operation, ProtocolPayload},
    policy::ClientApiKeyId,
    routing::ProviderKind,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    num::NonZeroU32,
    sync::atomic::Ordering,
    time::{Duration, SystemTime},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, path},
};

pub(super) fn session_request(protocol: &str, body: Value, context: Value) -> ProviderRequest {
    ProviderRequest::new(
        Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(protocol, body.as_object().unwrap().clone())
                .unwrap()
                .with_context(context.as_object().unwrap().clone()),
        )),
        request(false).candidate().clone(),
    )
}

fn context(key: &str, ttl: u64) -> gateway_core::engine::AttemptContext {
    context_with_policy(
        RequestAttemptContext::new(
            ModelRequestId::new("req_affinity").unwrap(),
            ClientApiKeyId::new(key).unwrap(),
        ),
        AccountSelectionPolicy::new(RotationStrategy::Smart, std::num::NonZeroU32::MIN)
            .with_account_affinity(AccountAffinity::Strict)
            .with_session_affinity_ttl(Duration::from_secs(ttl)),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn each_inbound_protocol_reuses_its_explicit_session_and_renews_ttl() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    for (protocol, body, metadata) in [
        (
            gateway_protocol::genai::GENAI_PROTOCOL,
            json!({"contents": []}),
            json!({"headers":{"x-http-session-id":"session-a"}}),
        ),
        (
            gateway_protocol::openai::OPENAI_PROTOCOL,
            json!({"input":[], "client_metadata":{"session_id":"session-a"}}),
            json!({}),
        ),
        (
            gateway_protocol::anthropic::ANTHROPIC_PROTOCOL,
            json!({"messages":[]}),
            json!({"opaque_request_headers":[["x-claude-code-session-id","c2Vzc2lvbi1h"]]}),
        ),
    ] {
        for ttl in [7200, 3600] {
            let stream = fixture
                .provider
                .clone()
                .execute(
                    session_request(protocol, body.clone(), metadata.clone()),
                    context("key_one", ttl),
                )
                .await
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_fixture"
            );
            drop(stream);
        }
    }
    let bindings = fixture.store.bindings.lock().unwrap();
    assert_eq!(
        bindings.len(),
        3,
        "each protocol has an independent identity namespace"
    );
    assert!(
        bindings
            .values()
            .all(|(_, ttl)| *ttl == Duration::from_secs(3600))
    );
}

#[tokio::test]
async fn same_session_is_isolated_between_client_keys() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    for key in ["key_one", "key_two"] {
        drop(
            fixture
                .provider
                .clone()
                .execute(
                    session_request(
                        gateway_protocol::genai::GENAI_PROTOCOL,
                        json!({"contents":[]}),
                        json!({"headers":{"x-session-id":"shared"}}),
                    ),
                    context(key, 3600),
                )
                .await
                .unwrap(),
        );
    }
    assert_eq!(fixture.store.bindings.lock().unwrap().len(), 2);
}

async fn select_genai_session(
    fixture: &Fixture,
    client_key: &str,
    body: Value,
    metadata: Value,
) -> ProviderStream {
    fixture
        .provider
        .clone()
        .execute(
            session_request(gateway_protocol::genai::GENAI_PROTOCOL, body, metadata),
            context_with_scope(
                RequestAttemptContext::new(
                    ModelRequestId::new("req_body_affinity").unwrap(),
                    ClientApiKeyId::new(client_key).unwrap(),
                ),
                AccountSelectionPolicy::new(RotationStrategy::RoundRobin, NonZeroU32::MIN)
                    .with_account_affinity(AccountAffinity::Strict),
                CancellationToken::new(),
                fixture.store.account_scope(),
            ),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn genai_body_session_aliases_keep_the_primary_and_share_one_binding() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    for body in [
        json!({"contents": [], "sessionId": "session-a"}),
        json!({"contents": [], "session_id": " session-a "}),
        json!({"contents": [], "sessionId": "session-a", "session_id": "other-body"}),
    ] {
        let stream = select_genai_session(&fixture, "key_one", body, json!({})).await;
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_fixture"
        );
        drop(stream);
    }
    assert_eq!(fixture.store.bindings.lock().unwrap().len(), 1);
    let other = select_genai_session(
        &fixture,
        "key_one",
        json!({"contents": [], "sessionId": "session-b"}),
        json!({}),
    )
    .await;
    assert_eq!(
        other.metadata().provider_account_id().as_str(),
        "acct_standby",
        "affinity hits must leave the next account for a new session"
    );
    drop(other);
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn genai_body_session_is_isolated_between_client_keys() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    let body = json!({"contents": [], "sessionId": "shared"});
    let first = select_genai_session(&fixture, "key_one", body.clone(), json!({})).await;
    let second = select_genai_session(&fixture, "key_two", body, json!({})).await;
    assert_ne!(
        first.metadata().provider_account_id(),
        second.metadata().provider_account_id()
    );
    assert_eq!(fixture.store.bindings.lock().unwrap().len(), 2);
    drop((first, second));
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn genai_session_headers_take_precedence_over_body_aliases() {
    for metadata in [
        json!({"headers": {"x-http-session-id": "header-session", "x-session-id": "other-header"}}),
        json!({"headers": {"x-session-id": "header-session"}}),
        json!({"opaque_request_headers": [["X-Http-Session-Id", "aGVhZGVyLXNlc3Npb24="]]}),
    ] {
        let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
        for body in [
            json!({"contents": [], "sessionId": "body-session", "session_id": "other-body"}),
            json!({"contents": []}),
        ] {
            let stream = select_genai_session(&fixture, "key_one", body, metadata.clone()).await;
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_fixture"
            );
            drop(stream);
        }
        assert_eq!(fixture.store.bindings.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_genai_body_sessions_do_not_create_affinity_bindings() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    for field in ["sessionId", "session_id"] {
        for session in [
            Value::Null,
            json!(42),
            json!(true),
            json!([]),
            json!({}),
            json!(""),
            json!("   "),
            json!("session\n-a"),
            json!("x".repeat(1025)),
        ] {
            drop(
                select_genai_session(
                    &fixture,
                    "key_one",
                    json!({"contents": [], field: session}),
                    json!({}),
                )
                .await,
            );
        }
    }
    assert!(fixture.store.bindings.lock().unwrap().is_empty());
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn invalid_genai_session_header_does_not_fall_back_to_body_identity() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    drop(
        select_genai_session(
            &fixture,
            "key_one",
            json!({"contents": [], "sessionId": "body-session"}),
            json!({"headers": {"x-http-session-id": " "}}),
        )
        .await,
    );
    assert!(fixture.store.bindings.lock().unwrap().is_empty());
}

#[tokio::test]
async fn requests_without_session_do_not_create_bindings() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    drop(
        fixture
            .provider
            .clone()
            .execute(request(false), context("key_one", 3600))
            .await
            .unwrap(),
    );
    assert_eq!(fixture.store.claims.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn binding_conflict_releases_the_lease_before_reselection() {
    let fixture = Fixture::new("http://127.0.0.1:1");
    fixture
        .store
        .reject_next_claim
        .store(true, Ordering::SeqCst);
    let stream = fixture
        .provider
        .clone()
        .execute(
            session_request(
                gateway_protocol::genai::GENAI_PROTOCOL,
                json!({"contents":[]}),
                json!({"headers":{"x-http-session-id":"session-a"}}),
            ),
            context("key_one", 3600),
        )
        .await
        .unwrap();
    assert_eq!(fixture.store.claims.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.store.active_leases(), 1);
    drop(stream);
    assert_eq!(fixture.store.active_leases(), 0);
}

async fn select_session(fixture: &Fixture, session: &str, mode: AccountAffinity) -> ProviderStream {
    let policy = AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::MIN)
        .with_account_affinity(mode)
        .with_queue(ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_secs(2),
        });
    let ctx = context_with_scope(
        RequestAttemptContext::new(
            ModelRequestId::new("req_affinity").unwrap(),
            ClientApiKeyId::new("key_fixture").unwrap(),
        ),
        policy,
        CancellationToken::new(),
        fixture.store.account_scope(),
    );
    fixture
        .provider
        .clone()
        .execute(
            session_request(
                gateway_protocol::genai::GENAI_PROTOCOL,
                json!({"contents":[{"parts":[{"text":"hello"}]}]}),
                json!({
                    "headers":{"x-http-session-id":session},
                    "opaque_request_headers":[["traceparent", "Zml4dHVyZS10cmFjZQ=="]],
                }),
            ),
            ctx,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn strict_session_waits_for_its_primary_while_another_session_uses_the_spare() {
    let fixture = Fixture::with_two_accounts("http://127.0.0.1:1");
    let select = |session| select_session(&fixture, session, AccountAffinity::Strict);
    let holder = select("session-a").await;
    let primary = holder.metadata().provider_account_id().clone();
    let waiting = select("session-a");
    tokio::pin!(waiting);
    assert!(futures::poll!(&mut waiting).is_pending());

    let other = select("session-b").await;
    assert_ne!(other.metadata().provider_account_id(), &primary);
    assert_eq!(fixture.store.active_leases(), 2);
    drop(holder);

    let resumed = waiting.await;
    assert_eq!(resumed.metadata().provider_account_id(), &primary);
    drop((resumed, other));
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn preferred_session_can_send_with_the_spare_then_returns_to_its_primary() {
    let upstream = MockServer::start().await;
    for name in ["fixture", "standby"] {
        Mock::given(path("/v1internal:generateContent"))
            .and(header("authorization", format!("Bearer {name}-access")))
            .and(body_partial_json(json!({"project": format!("{name}-project")})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}
            })))
            .expect(1)
            .mount(&upstream)
            .await;
    }
    let fixture = Fixture::with_two_accounts(&upstream.uri());
    let select = || select_session(&fixture, "session-a", AccountAffinity::Preferred);
    let holder = select().await;
    let primary = holder.metadata().provider_account_id().clone();

    let overflow = select().await;
    assert_ne!(overflow.metadata().provider_account_id(), &primary);
    assert!(
        overflow
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .iter()
            .any(|event| event.has_client_event())
    );
    assert_eq!(
        fixture
            .store
            .bindings
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .0
            .account_id(),
        &primary
    );

    drop(holder);
    let recovered = select().await;
    assert_eq!(recovered.metadata().provider_account_id(), &primary);
    assert!(
        recovered
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .iter()
            .any(|event| event.has_client_event())
    );
    upstream.verify().await;
    let sent = upstream.received_requests().await.unwrap();
    assert!(sent[0].headers.get("traceparent").is_none());
    assert_eq!(sent[1].headers["traceparent"], "fixture-trace");
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn disabled_primary_migrates_the_session_binding() {
    let upstream = MockServer::start().await;
    Mock::given(path("/v1internal:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": {"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}
        })))
        .expect(2)
        .mount(&upstream)
        .await;
    let fixture = Fixture::with_two_accounts(&upstream.uri());
    let select = || select_session(&fixture, "session-a", AccountAffinity::Strict);
    let first = select().await;
    let primary = first.metadata().provider_account_id().clone();
    drop(first);

    fixture.store.set_enabled(&primary, false).await.unwrap();
    let replacement = select().await;
    let account = replacement.metadata().provider_account_id().clone();
    assert_ne!(account, primary);
    replacement.try_collect::<Vec<_>>().await.unwrap();

    fixture.store.set_enabled(&primary, true).await.unwrap();
    let same_account = select().await;
    assert_eq!(same_account.metadata().provider_account_id(), &account);
    same_account.try_collect::<Vec<_>>().await.unwrap();
    assert_eq!(
        fixture
            .store
            .bindings
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .0
            .account_id(),
        &account
    );
    let sent = upstream.received_requests().await.unwrap();
    assert!(sent[0].headers.get("traceparent").is_none());
    assert_eq!(sent[1].headers["traceparent"], "fixture-trace");
    upstream.verify().await;
    assert_eq!(fixture.store.active_leases(), 0);
}

#[tokio::test]
async fn sessionless_retry_strips_trace_only_for_a_different_known_state_owner() {
    let upstream = MockServer::start().await;
    Mock::given(path("/v1internal:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": {"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}
        })))
        .expect(3)
        .mount(&upstream)
        .await;
    let fixture = Fixture::new(&upstream.uri());
    for owner in [None, Some("acct_fixture"), Some("acct_standby")] {
        let ctx = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_retry_trace").unwrap(),
                ClientApiKeyId::new("key_fixture").unwrap(),
            ),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(30),
            AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::MIN),
            AccountAttemptContext::new(
                BTreeSet::new(),
                None,
                owner.map(|account| {
                    ProviderAccountStateOwner::new(
                        ProviderKind::new("antigravity").unwrap(),
                        ProviderAccountId::new(account).unwrap(),
                    )
                }),
            )
            .with_account_scope(fixture.store.account_scope()),
            None,
            CancellationToken::new(),
        );
        fixture
            .provider
            .clone()
            .execute(
                session_request(
                    gateway_protocol::genai::GENAI_PROTOCOL,
                    json!({"contents": []}),
                    json!({"opaque_request_headers":[["traceparent", "Zml4dHVyZS10cmFjZQ=="]]}),
                ),
                ctx,
            )
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
    }
    let sent = upstream.received_requests().await.unwrap();
    assert_eq!(sent[0].headers["traceparent"], "fixture-trace");
    assert_eq!(sent[1].headers["traceparent"], "fixture-trace");
    assert!(sent[2].headers.get("traceparent").is_none());
    assert!(fixture.store.bindings.lock().unwrap().is_empty());
    assert_eq!(fixture.store.active_leases(), 0);
    upstream.verify().await;
}
