//! Live 引导模型事实与固定账号重连授权回归

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::account::{
    AccountModelAccess, AccountModelAccessMode, FastMode, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::engine::middleware::{
    MiddlewareContext, MiddlewareError, MiddlewareHeader, MiddlewareNext, MiddlewarePlan,
    MiddlewareRequest, MiddlewareResponse,
};
use gateway_core::engine::provider::{Provider, ProviderRequest};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::live::{LiveGatewayErrorKind, LiveSidebandRequest, LiveSidebandStyle};
use gateway_core::operation::{Operation, ProviderHttpMethod, ProviderHttpRequest, RawHttpPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ClientRoutingScope, ConfigRevision, FrozenAccountScope, ProviderKind, RoutingContext,
    RuntimeAccount, RuntimeAccountDirectory, RuntimeSnapshot, UpstreamModelId,
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::contract::{context, context_with_middleware, create_account, provider_with_base_url};
use crate::support::MemoryAccountStore;

const MODEL: &str = "gpt-live-1-codex";
const ACCOUNT: &str = "acct_provider_contract";
const CALL: &str = "call_live_regression";

#[tokio::test]
async fn outbound_live_create_sends_final_body_headers_method_and_target() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let original = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/plugin-live"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .insert_header("content-type", "application/sdp")
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&target)
        .await;
    let uri = format!("{}/plugin-live", target.uri());
    let mut stream = provider_with_base_url(&store, original.uri())
        .execute(
            live_request(),
            super::contract::outbound_context("req_outbound_live", move |request| {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["sdp"],
                    "v=0"
                );
                assert!(!request.header_map().unwrap().contains_key("accept"));
                request.target = gateway_core::engine::middleware::outbound::Target::Http {
                    method: reqwest::Method::PUT,
                    uri: uri.parse().unwrap(),
                };
                request
                    .headers
                    .retain(|header| !matches!(header.name(), "user-agent" | "accept"));
                request.body = Bytes::from_static(b"\xffopaque live request");
                Ok(())
            }),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert!(original.received_requests().await.unwrap().is_empty());
    let requests = target.received_requests().await.unwrap();
    assert_eq!(requests[0].body, b"\xffopaque live request");
    assert!(!requests[0].headers.contains_key("user-agent"));
    assert_eq!(requests[0].headers["accept"], "*/*");
}

fn scope(model_access: AccountModelAccess) -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new(ACCOUNT).unwrap(),
            RuntimeAccount::new(ProviderKind::new("openai").unwrap(), BTreeSet::new())
                .with_model_access(model_access),
        )]))),
        ClientRoutingScope::all_accounts(),
    ))
}

fn live_request() -> ProviderRequest {
    live_request_with_headers(Vec::new())
}

fn live_request_with_headers(
    headers: Vec<gateway_core::operation::ProviderHttpHeader>,
) -> ProviderRequest {
    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new(MODEL).unwrap();
    let operation = Operation::ProviderHttp(
        ProviderHttpRequest::new(
            "realtime-calls",
            ProviderHttpMethod::Post,
            Some("intent=quicksilver&architecture=avas".into()),
            headers,
            RawHttpPayload::new(
                "openai",
                Bytes::from(json!({"sdp":"v=0", "session":{"model":MODEL}}).to_string()),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, "smart", BTreeMap::new(), None, None),
        vec![provider.clone()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan_provider_endpoint(
            &provider,
            Some(&model),
            &operation,
            scope(AccountModelAccess::default()),
            &RoutingContext::default(),
        )
        .unwrap();
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

#[derive(Debug)]
struct ModelMiddleware;
impl MiddlewarePlan for ModelMiddleware {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.model(), Some(MODEL));
        next.run(request)
    }
}

async fn bootstrap() -> (Arc<dyn Provider>, Arc<MemoryAccountStore>, MockServer) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/realtime/calls"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .insert_header("content-type", "application/sdp")
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let provider = crate::admin::initialized_test_provider(store.clone(), upstream.uri()).await;
    let request = live_request();
    let candidate = request.candidate().clone();
    let mut stream = provider
        .clone()
        .execute(
            request,
            context_with_middleware(
                "req_live_regression",
                Arc::new(ModelMiddleware),
                FastMode::Default,
            ),
        )
        .await
        .expect("live stream");
    assert!(
        stream.metadata().confirms(&candidate),
        "Core must accept the frozen provider/model facts"
    );
    assert!(
        upstream.received_requests().await.unwrap().is_empty(),
        "bootstrap must remain cold before Core accepts metadata"
    );
    while let Some(event) = stream.next().await {
        event.expect("bootstrap response");
    }
    (provider, store, upstream)
}

#[tokio::test]
async fn live_bootstrap_preserves_model_through_middleware_and_metadata() {
    let (_provider, _store, upstream) = bootstrap().await;
    upstream.verify().await;
}

#[tokio::test]
async fn live_create_uses_outbound_rewrite_without_changing_call_ownership() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/realtime/calls"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .insert_header("content-type", "application/sdp")
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = crate::admin::initialized_test_provider(store, server.uri()).await;
    let request =
        live_request_with_headers(vec![gateway_core::operation::ProviderHttpHeader::new(
            "x-codex-turn-metadata",
            Bytes::from(json!({"installation_id":"client-installation"}).to_string()),
        )]);
    let mut stream = provider
        .execute(
            request,
            super::contract::outbound_context("req_live_outbound", |request| {
                request
                    .headers
                    .retain(|header| header.name() != "x-codex-turn-metadata");
                request.headers.push(MiddlewareHeader::new(
                    "x-codex-turn-metadata",
                    Bytes::from(json!({"installation_id":"live-plugin-installation"}).to_string()),
                ));
                Ok(())
            }),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert_eq!(stream.metadata().provider_account_id().as_str(), ACCOUNT);
    let requests = server.received_requests().await.unwrap();
    let metadata: serde_json::Value = serde_json::from_str(
        requests[0].headers["x-codex-turn-metadata"]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["installation_id"], "live-plugin-installation");
}

#[tokio::test]
async fn live_sideband_rejects_revoked_scope_and_model_without_claiming_call() {
    let (provider, _store, _upstream) = bootstrap().await;
    let gateway = provider.live_gateway().unwrap();
    let denied_model = scope(
        AccountModelAccess::new(AccountModelAccessMode::Denylist, vec![MODEL.to_owned()]).unwrap(),
    );
    let absent_account = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::new())),
        ClientRoutingScope::all_accounts(),
    ));
    let key = ClientApiKeyId::new("key_openai_contract").unwrap();
    // 同一 call 连续拒绝不能遗留 claim，后续请求仍应得到权限错误而不是 busy
    for account_scope in [&absent_account, &denied_model, &absent_account] {
        let error = gateway
            .open_sideband(LiveSidebandRequest {
                call_id: CALL,
                client_api_key_id: &key,
                account_scope,
                style: LiveSidebandStyle::Live,
                protocol_headers: Vec::new(),
                subprotocols: Vec::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), LiveGatewayErrorKind::OwnerMismatch);
        assert_eq!(error.status(), 403);
    }
}

#[tokio::test]
async fn live_sideband_rejects_disabled_account_even_before_scope_refresh() {
    let (provider, store, _upstream) = bootstrap().await;
    store
        .set_enabled(&ProviderAccountId::new(ACCOUNT).unwrap(), false)
        .await
        .unwrap();
    let gateway = provider.live_gateway().unwrap();
    let key = ClientApiKeyId::new("key_openai_contract").unwrap();
    let allowed = scope(AccountModelAccess::default());
    for _ in 0..2 {
        let error = gateway
            .open_sideband(LiveSidebandRequest {
                call_id: CALL,
                client_api_key_id: &key,
                account_scope: &allowed,
                style: LiveSidebandStyle::Live,
                protocol_headers: Vec::new(),
                subprotocols: Vec::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.status(), 403);
    }
}

#[tokio::test]
async fn live_bootstrap_rejects_missing_planned_model_before_upstream_send() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let upstream = MockServer::start().await;
    let provider = provider_with_base_url(&store, upstream.uri());
    let request = live_request();
    let provider_kind = ProviderKind::new("openai").unwrap();
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, "smart", BTreeMap::new(), None, None),
        vec![provider_kind.clone()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan_provider_endpoint(
            &provider_kind,
            None,
            request.operation(),
            scope(AccountModelAccess::default()),
            &RoutingContext::default(),
        )
        .unwrap();
    let result = provider
        .execute(
            ProviderRequest::new(request.operation().clone(), plan.candidates()[0].clone()),
            context("req_live_missing_model", CancellationToken::new()),
        )
        .await;
    assert!(result.is_err());
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

fn live_protocol_headers() -> Vec<gateway_core::operation::ProviderHttpHeader> {
    [
        ("originator", "foreign-originator"),
        ("User-Agent", "foreign-agent"),
        ("authorization", "Bearer foreign"),
        ("openai-organization", "foreign-org"),
        ("OpenAI-Project", "foreign-project"),
        ("ChatGPT-Account-Id", "foreign-account"),
        ("x-openai-internal-codex-residency", "foreign-region"),
        ("x-oai-attestation", "foreign-attestation"),
        ("x-forwarded-for", "192.0.2.1"),
        ("x-stainless-runtime", "node"),
        ("traceparent", "client-trace"),
        ("tracestate", "client-state"),
        ("ws_request_header_traceparent", "client-ws-trace"),
        ("x-codex-turn-state", "foreign-turn-state"),
        ("x-codex-routing-hint", "foreign-route"),
        ("openai-alpha", "live-test"),
        ("voice-session-id", "voice-product-session"),
        ("x-session-id", "realtime-api-session"),
        ("session-id", "client-session"),
        ("thread-id", "client-session"),
        ("x-test-business", "first"),
        ("x-test-business", "second"),
        ("x-codex-turn-metadata", r#"{"session_id":"client-session","account_id":"foreign-account","installation_id":"foreign-installation","traceparent":"client-trace","future":true}"#),
    ]
    .into_iter()
    .map(|(name, value)| gateway_core::operation::ProviderHttpHeader::new(name, Bytes::from_static(value.as_bytes())))
    .collect()
}

fn assert_live_correlation(headers: &reqwest::header::HeaderMap) {
    assert_eq!(headers["voice-session-id"], "voice-product-session");
    assert_eq!(headers["x-session-id"], "realtime-api-session");
    let session = headers["session-id"].to_str().unwrap();
    assert_ne!(session, "client-session");
    uuid::Uuid::parse_str(session).unwrap();
    assert_eq!(headers["thread-id"], session);
    assert_eq!(headers["openai-alpha"], "live-test");
    assert_eq!(
        headers
            .get_all("x-test-business")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    for name in [
        "openai-organization",
        "x-oai-attestation",
        "x-forwarded-for",
        "x-stainless-runtime",
        "traceparent",
        "tracestate",
        "ws_request_header_traceparent",
        "x-codex-turn-state",
        "x-codex-routing-hint",
    ] {
        assert!(!headers.contains_key(name), "leaked {name}");
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(headers["x-codex-turn-metadata"].as_bytes()).unwrap();
    assert_eq!(metadata["session_id"], session);
    assert_eq!(metadata["future"], true);
    assert_ne!(metadata["installation_id"], "foreign-installation");
    assert!(metadata.get("account_id").is_none());
    assert!(metadata.get("traceparent").is_none());
}

#[tokio::test]
async fn live_should_scope_correlation_to_account_and_preserve_business_headers() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/realtime/calls"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .set_body_raw("v=0\r\n", "application/sdp")
                .append_header("x-test-response", "first")
                .append_header(
                    "x-test-response",
                    reqwest::header::HeaderValue::from_bytes(b"\x80second").unwrap(),
                )
                .insert_header("set-cookie", "upstream-private=1")
                .insert_header("openai-organization", "upstream-org")
                .insert_header("connection", "x-upstream-hop")
                .insert_header("x-upstream-hop", "private"),
        )
        .expect(4)
        .mount(&upstream)
        .await;
    // 独立账号安装身份分别发送相同客户端标识，同账号重复请求应稳定、不同账号域应隔离
    for _ in 0..2 {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, ACCOUNT).await;
        let provider = crate::admin::initialized_test_provider(store, upstream.uri()).await;
        for _ in 0..2 {
            let mut headers = live_protocol_headers();
            headers.push(gateway_core::operation::ProviderHttpHeader::new(
                "x-test-opaque",
                Bytes::from_static(b"\x80request"),
            ));
            let mut stream = provider
                .clone()
                .execute(
                    live_request_with_headers(headers),
                    context("req_live_headers", CancellationToken::new()),
                )
                .await
                .unwrap();
            let mut response_headers = Vec::new();
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                if let Some(observation) = event.response_observation() {
                    response_headers.extend_from_slice(observation.client_headers());
                }
            }
            let values = response_headers
                .iter()
                .filter(|header| header.name() == "x-test-response")
                .map(|header| header.value())
                .collect::<Vec<_>>();
            assert_eq!(values, [b"first".as_slice(), b"\x80second".as_slice()]);
            assert!(
                response_headers
                    .iter()
                    .any(|header| header.name() == "location")
            );
            assert!(
                response_headers
                    .iter()
                    .any(|header| header.name() == "content-type"
                        && header.value().as_ref() == b"application/sdp")
            );
            assert!(response_headers.iter().all(|header| {
                ![
                    "set-cookie",
                    "openai-organization",
                    "connection",
                    "x-upstream-hop",
                    "content-length",
                ]
                .contains(&header.name())
            }));
        }
    }
    let requests = upstream.received_requests().await.unwrap();
    for request in &requests {
        assert_live_correlation(&request.headers);
        assert_eq!(
            request.headers["authorization"],
            format!("Bearer at-{ACCOUNT}")
        );
        assert_ne!(request.headers["originator"], "foreign-originator");
        assert_eq!(request.headers["x-test-opaque"].as_bytes(), b"\x80request");
    }
    assert_eq!(
        requests[0].headers["session-id"],
        requests[1].headers["session-id"]
    );
    assert_eq!(
        requests[2].headers["session-id"],
        requests[3].headers["session-id"]
    );
    assert_ne!(
        requests[0].headers["session-id"],
        requests[2].headers["session-id"]
    );
}

// 自定义 CA 只注入子进程，避免并行测试修改全局 TLS 配置
#[cfg(target_os = "linux")]
#[test]
fn live_call_should_pin_profile_but_reload_credentials_and_proxy() {
    const CHILD: &str = "CPR_LIVE_PROFILE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    verify_live_call_identity(),
                )
                .await
                .expect("local Live identity verification timed out");
            });
        println!("live-profile-verified");
        return;
    }
    use openssl::{
        asn1::Asn1Time,
        hash::MessageDigest,
        pkey::PKey,
        rsa::Rsa,
        x509::{
            X509, X509NameBuilder,
            extension::{BasicConstraints, SubjectAlternativeName},
        },
    };
    let directory = tempfile::tempdir().unwrap();
    let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", "Live local test").unwrap();
    let name = name.build();
    let mut cert = X509::builder().unwrap();
    cert.set_version(2).unwrap();
    cert.set_subject_name(&name).unwrap();
    cert.set_issuer_name(&name).unwrap();
    cert.set_pubkey(&key).unwrap();
    cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    cert.append_extension(BasicConstraints::new().critical().build().unwrap())
        .unwrap();
    let san = SubjectAlternativeName::new()
        .dns("api.openai.com")
        .build(&cert.x509v3_context(None, None))
        .unwrap();
    cert.append_extension(san).unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    let cert = cert.build();
    let ca = directory.path().join("ca.pem");
    std::fs::write(&ca, cert.to_pem().unwrap()).unwrap();
    std::fs::write(directory.path().join("server.der"), cert.to_der().unwrap()).unwrap();
    std::fs::write(
        directory.path().join("key.der"),
        key.private_key_to_pkcs8().unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "provider::live::live_call_should_pin_profile_but_reload_credentials_and_proxy",
            "--nocapture",
        ])
        .env(CHILD, directory.path())
        .env(provider_openai::transport::tls::CODEX_CA_CERT_ENV, &ca)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("live-profile-verified"));
}

#[cfg(target_os = "linux")]
async fn verify_live_call_identity() {
    use gateway_core::account::{
        CredentialCasOutcome, CredentialCasUpdate, OpaqueProviderData, OutboundProxy,
        ProviderAccountUpdate,
    };
    use gateway_core::live::LiveHangupRequest;
    use provider_openai::credential::{CodexCredentialAdmin, ImportCodexOAuthCredential};
    use provider_openai::transport::profile::CodexWireProfile;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    let profile = CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        exact_user_agent: Some("codex_cli_rs/0.160.0 (Linux; x86_64) live-test".to_owned()),
        codex_version: "0.160.0".to_owned(),
        residency: Some("us".to_owned()),
        ..CodexWireProfile::default()
    };
    let opaque_profile = |profile: &CodexWireProfile| {
        OpaqueProviderData::new(
            serde_json::to_value(profile)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/realtime/calls"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let provider = crate::admin::initialized_test_provider(store.clone(), upstream.uri()).await;
    let mut stream = provider
        .clone()
        .execute(
            live_request_with_headers(live_protocol_headers()),
            gateway_core::engine::AttemptContext::new(
                gateway_core::engine::RequestAttemptContext::new(
                    gateway_core::engine::ModelRequestId::new("req_live_frozen_profile").unwrap(),
                    ClientApiKeyId::new("key_openai_contract").unwrap(),
                )
                .with_request_profile(Some(opaque_profile(&profile))),
                std::num::NonZeroU32::new(1).unwrap(),
                std::time::SystemTime::now() + std::time::Duration::from_secs(30),
                crate::support::account_policy(),
                gateway_core::engine::AccountAttemptContext::new(BTreeSet::new(), None, None)
                    .with_account_scope(scope(AccountModelAccess::default())),
                None,
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    drop(stream);
    let creation = upstream.received_requests().await.unwrap().pop().unwrap();
    assert_live_profile(&creation.headers, &profile, &format!("Bearer at-{ACCOUNT}"));

    assert_live_correlation(&creation.headers);
    let scoped_session = creation.headers["session-id"].clone();

    let mut changed_profile = profile.clone();
    changed_profile.exact_user_agent = Some("changed-key-profile".to_owned());
    changed_profile.originator = "changed-originator".to_owned();
    changed_profile.residency = None;
    let current_scope = Arc::new(
        (*scope(AccountModelAccess::default()))
            .clone()
            .with_request_profiles(BTreeMap::from([(
                ProviderKind::new("openai").unwrap(),
                opaque_profile(&changed_profile),
            )])),
    );
    let key_id = ClientApiKeyId::new("key_openai_contract").unwrap();
    let gateway = provider.live_gateway().unwrap();
    for iteration in 0..3 {
        // 每次使用不同出口与凭据，断言通话只冻结画像，不冻结账号运行状态
        let replacement = CodexCredentialAdmin
            .prepare_import(ImportCodexOAuthCredential {
                account_id: ACCOUNT.to_owned(),
                name: ACCOUNT.to_owned(),
                secret: crate::support::secret(&format!("rotated-live-{iteration}")),
                verified_account: crate::support::profile(&format!("chatgpt-{ACCOUNT}")),
                next_refresh_at: None,
                enabled: true,
            })
            .unwrap();
        let account = store.account(ACCOUNT).unwrap();
        let current = store
            .load_credential(account.id(), account.revision())
            .await
            .unwrap();
        let replacement_credential =
            provider_openai::credential::CodexCredentialCodec::preserve_installation_id(
                &replacement.credential,
                &current.credential,
            )
            .unwrap();
        let outcome = store
            .compare_and_swap_credential(
                CredentialCasUpdate::new(
                    account.id().clone(),
                    account.revision(),
                    ProviderAccountUpdate {
                        account_id: account.id().clone(),
                        name: ACCOUNT.to_owned(),
                        email: None,
                        plan_type: None,
                    },
                    replacement_credential,
                    true,
                    None,
                    None,
                )
                .unwrap()
                .preserving_profile(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, CredentialCasOutcome::Updated(_)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        store.set_egress(
            ACCOUNT,
            Some(
                OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap()))
                    .unwrap(),
            ),
            None,
        );
        let expected = profile.clone();
        let expected_session = scoped_session.clone();
        let server = tokio::spawn(async move {
            let mut tls = accept_live_proxy(listener).await;
            if iteration < 2 {
                let socket = crate::transport::accept_codex_test_websocket_with(
                    tls,
                    |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     _response| {
                        assert_eq!(request.uri().path(), format!("/v1/live/{CALL}"));
                        assert_live_profile(
                            request.headers(),
                            &expected,
                            &format!("Bearer rotated-live-{iteration}"),
                        );
                        assert_live_correlation(request.headers());
                        assert_eq!(request.headers()["session-id"], expected_session);
                    },
                )
                .await;
                drop(socket);
            } else {
                let head = read_live_head(&mut tls).await;
                assert!(
                    head.starts_with(&format!("POST /v1/realtime/calls/{CALL}/hangup HTTP/1.1"))
                );
                let mut headers = reqwest::header::HeaderMap::new();
                for line in head.split("\r\n").skip(1) {
                    if let Some((name, value)) = line.split_once(':') {
                        headers.append(
                            reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                            reqwest::header::HeaderValue::from_str(value.trim()).unwrap(),
                        );
                    }
                }
                assert_live_profile(
                    &headers,
                    &expected,
                    &format!("Bearer rotated-live-{iteration}"),
                );
                assert_live_correlation(&headers);
                assert_eq!(headers["session-id"], expected_session);
                tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Test-Response: first\r\nX-Test-Response: \x80second\r\nSet-Cookie: private=1\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        let protocol_headers = live_protocol_headers();
        if iteration < 2 {
            let relay = gateway
                .open_sideband(LiveSidebandRequest {
                    call_id: CALL,
                    client_api_key_id: &key_id,
                    account_scope: &current_scope,
                    style: LiveSidebandStyle::Live,
                    protocol_headers,
                    subprotocols: Vec::new(),
                })
                .await
                .expect("sideband with frozen profile and current credential");
            drop(relay);
        } else {
            let outcome = gateway
                .hangup(LiveHangupRequest {
                    call_id: CALL,
                    client_api_key_id: &key_id,
                    content_type: None,
                    protocol_headers,
                    body: Bytes::new(),
                })
                .await
                .expect("hangup through current account proxy");
            assert_eq!(outcome.status, 200);
            assert_eq!(
                outcome
                    .headers
                    .iter()
                    .filter(|header| header.name() == "x-test-response")
                    .map(|header| header.value())
                    .collect::<Vec<_>>(),
                [b"first".as_slice(), b"\x80second".as_slice()]
            );
            assert!(
                outcome
                    .headers
                    .iter()
                    .all(|header| header.name() != "set-cookie")
            );
        }
        server.await.unwrap();
    }
}

#[cfg(target_os = "linux")]
fn assert_live_profile(
    headers: &reqwest::header::HeaderMap,
    profile: &provider_openai::transport::profile::CodexWireProfile,
    authorization: &str,
) {
    assert_eq!(headers["user-agent"], profile.user_agent());
    assert_eq!(headers["originator"], profile.originator);
    assert_eq!(headers["version"], profile.codex_version);
    assert_eq!(headers["x-openai-internal-codex-residency"], "us");
    assert_eq!(headers["authorization"], authorization);
    assert_eq!(headers["chatgpt-account-id"], format!("chatgpt-{ACCOUNT}"));
    for name in [
        "openai-organization",
        "openai-project",
        "x-oai-attestation",
        "x-codex-routing-hint",
        "x-codex-turn-state",
        "openai-beta",
    ] {
        assert!(
            !headers.contains_key(name),
            "unexpected Live header: {name}"
        );
    }
}

#[cfg(target_os = "linux")]
async fn accept_live_proxy(
    listener: tokio::net::TcpListener,
) -> tokio_rustls::server::TlsStream<tokio::net::TcpStream> {
    use tokio::io::AsyncWriteExt;
    let directory =
        std::path::PathBuf::from(std::env::var_os("CPR_LIVE_PROFILE_TEST_CHILD").unwrap());
    let (mut stream, _) = listener.accept().await.unwrap();
    let connect = read_live_head(&mut stream).await;
    assert!(connect.starts_with("CONNECT api.openai.com:443 HTTP/1.1"));
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                std::fs::read(directory.join("server.der")).unwrap(),
            )],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                std::fs::read(directory.join("key.der")).unwrap().into(),
            ),
        )
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
        .accept(stream)
        .await
        .unwrap()
}

#[cfg(target_os = "linux")]
async fn read_live_head(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> String {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < 16_384, "bounded local HTTP header");
        head.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(head).unwrap()
}

#[tokio::test]
async fn descendant_live_creation_waits_and_follows_root_account_migration() {
    use super::contract::{
        generate_with_session_context, planned_request,
        provider_with_affinity_and_base_url_and_leases,
    };
    use crate::support::{MemorySessionAffinity, TestLeaseCoordinator};
    use gateway_core::operation::ProviderHttpHeader;
    use std::time::Duration;

    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/realtime/calls"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("location", format!("/v1/live/{CALL}"))
                .insert_header("content-type", "application/sdp")
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let provider = provider_with_affinity_and_base_url_and_leases(
        &store,
        affinity.clone(),
        upstream.uri(),
        leases.clone(),
    );
    let root = || {
        planned_request(
            "openai",
            Operation::Generate(generate_with_session_context(
                "live-root",
                Some("live-root"),
                None,
            )),
        )
    };
    drop(
        provider
            .clone()
            .execute(root(), context("req_live_root", CancellationToken::new()))
            .await
            .unwrap(),
    );
    create_account(&store, "acct_subagent_b").await;
    leases
        .busy_accounts
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new(ACCOUNT).unwrap());
    let live = live_request_with_headers(vec![
        ProviderHttpHeader::new("session-id", Bytes::from_static(b"live-root")),
        ProviderHttpHeader::new("thread-id", Bytes::from_static(b"live-child")),
        ProviderHttpHeader::new("x-session-id", Bytes::from_static(b"unrelated-live-call")),
    ]);
    let mut pending = Box::pin(
        provider
            .clone()
            .execute(live, context("req_live_child", CancellationToken::new())),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), pending.as_mut())
            .await
            .is_err()
    );
    assert!(upstream.received_requests().await.unwrap().is_empty());
    drop(
        provider
            .clone()
            .execute(
                root(),
                context("req_live_root_migrate", CancellationToken::new()),
            )
            .await
            .unwrap(),
    );
    let mut stream = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_subagent_b"
    );
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert_eq!(affinity.binding_count(), 1);
    assert_eq!(
        upstream.received_requests().await.unwrap()[0].headers["chatgpt-account-id"]
            .to_str()
            .unwrap(),
        "chatgpt-acct_subagent_b"
    );
    upstream.verify().await;
}
