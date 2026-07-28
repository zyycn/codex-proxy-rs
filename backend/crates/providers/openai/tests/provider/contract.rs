use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use futures::StreamExt;
use gateway_core::engine::credential::{
    AccountAvailability, AccountFeedbackStats, ProviderAccountId,
};
use gateway_core::engine::provider::{Provider as _, ProviderRequest};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId,
    RequestAttemptContext, UpstreamSendState,
};
use gateway_core::error::ProviderErrorKind;
use gateway_core::event::GatewayEvent;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext,
    RuntimeSnapshot, UpstreamModelId,
};
use provider_openai::credential::{
    CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialQuotaService,
    CodexCredentialSelector, ImportCodexOAuthCredential,
};
use provider_openai::transport::CodexWebSocketPool;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::{CodexProvider, OFFICIAL_CODEX_BASE_URL};
use serde_json::{Map, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{
    MemoryAccountStore, MemorySessionAffinity, MemorySessionExclusions, TestLeaseCoordinator,
    account_policy, agent_identity_service_with_pool, catalog_cache, profile, secret,
};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../transport/fixtures/official_models_snapshot.json");

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "provider-contract".to_owned(),
        verified_at: Utc::now(),
    })
}

fn provider(store: &Arc<MemoryAccountStore>) -> CodexProvider {
    provider_with_affinity(store, Arc::new(MemorySessionAffinity::default()))
}

fn provider_with_affinity(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
) -> CodexProvider {
    provider_with_affinity_and_base_url(store, session_affinity, OFFICIAL_CODEX_BASE_URL.to_owned())
}

fn provider_with_base_url(store: &Arc<MemoryAccountStore>, base_url: String) -> CodexProvider {
    provider_with_affinity_and_base_url(store, Arc::new(MemorySessionAffinity::default()), base_url)
}

fn provider_with_affinity_and_base_url(
    store: &Arc<MemoryAccountStore>,
    session_affinity: Arc<MemorySessionAffinity>,
    base_url: String,
) -> CodexProvider {
    let profile = wire_profile();
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let websocket_pool = Arc::new(CodexWebSocketPool::default());
    let agent_identity = agent_identity_service_with_pool(store, Arc::clone(&websocket_pool));
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        Arc::clone(&agent_identity),
        catalog_cache(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        base_url.clone(),
        Arc::clone(&agent_identity),
    ));
    let account_feedback = Arc::new(AccountFeedbackStats::default());
    let selector = Arc::new(CodexCredentialSelector::new(
        ProviderKind::new("openai").expect("provider"),
        store.repository(),
        Arc::new(TestLeaseCoordinator::default()),
        session_affinity,
        Arc::new(MemorySessionExclusions::default()),
        Arc::clone(&catalog),
        Arc::clone(&quota),
        Arc::clone(&agent_identity),
        Arc::clone(&account_feedback),
        CodexCookiePolicy::official().expect("cookie policy"),
        true,
    ));

    CodexProvider::new(
        selector,
        catalog,
        quota,
        agent_identity,
        account_feedback,
        http,
        profile,
        base_url,
        websocket_pool,
    )
    .expect("official OpenAI provider")
}

async fn create_account(store: &Arc<MemoryAccountStore>, id: &str) {
    create_account_with_enabled(store, id, true).await;
}

async fn create_account_with_enabled(store: &Arc<MemoryAccountStore>, id: &str, enabled: bool) {
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: id.to_owned(),
            name: id.to_owned(),
            secret: secret(&format!("at-{id}")),
            verified_account: profile(&format!("chatgpt-{id}")),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled,
        })
        .await;
}

fn generate_operation() -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
            ]),
        )
        .expect("OpenAI payload"),
    ))
}

fn http_generate_operation() -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))]));
    Operation::Generate(GenerateRequest::from_protocol_payload(payload))
}

fn planned_request(provider_name: &str, operation: Operation) -> ProviderRequest {
    let provider = ProviderKind::new(provider_name).expect("provider");
    let upstream_model = UpstreamModelId::new("gpt-5.4").expect("upstream model");
    let public_model = PublicModelId::new(upstream_model.as_str()).expect("public model");
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).expect("revision"),
        account_policy(),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            upstream_model,
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000)),
        )],
        Vec::new(),
    )
    .expect("snapshot");
    let plan = snapshot
        .plan(&public_model, &operation, &RoutingContext::default())
        .expect("routing plan");

    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn context(request_id: &str, cancellation: CancellationToken) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::<ProviderAccountId>::new(), None, None),
        None,
        cancellation,
    )
}

fn diagnostic_context(request_id: &str, account_id: &str) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).expect("request id"),
            ClientApiKeyId::new("key_openai_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::diagnostic(
            BTreeSet::new(),
            ProviderAccountId::new(account_id).expect("account id"),
            None,
        ),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn openai_provider_rejects_a_foreign_provider_candidate_before_account_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    let result = provider(&store)
        .execute(
            planned_request("xai", generate_operation()),
            context("req_foreign_provider", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("foreign provider candidate must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn cancelled_attempt_fails_before_account_selection_or_upstream_send() {
    let store = Arc::new(MemoryAccountStore::default());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let result = provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_cancelled", cancellation),
        )
        .await;
    let Err(error) = result else {
        panic!("cancelled attempt must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn openai_provider_keeps_a_compaction_trigger_as_a_regular_generate_request() {
    let store = Arc::new(MemoryAccountStore::default());
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                (
                    "input".to_owned(),
                    json!([
                        {"type": "message", "role": "user", "content": "hello"},
                        {"type": "compaction_trigger"}
                    ]),
                ),
            ]),
        )
        .expect("OpenAI payload"),
    ));
    let result = provider(&store)
        .execute(
            planned_request("openai", operation),
            context("req_compaction", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("missing OpenAI account must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn generate_without_an_eligible_openai_account_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    let result = provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_no_account", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("missing OpenAI account must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn disabled_account_is_excluded_from_normal_scheduling() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account_with_enabled(&store, "acct_disabled_scheduling", false).await;

    let result = provider(&store)
        .execute(
            planned_request("openai", generate_operation()),
            context("req_disabled_scheduling", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("disabled account must not be scheduled normally")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn opaque_provider_options_do_not_change_openai_account_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
            (
                "provider_options".to_owned(),
                json!({"providers": {"openai": {"transport": "unsupported"}}}),
            ),
        ]),
    )
    .expect("OpenAI payload");
    let generation = GenerateRequest::from_protocol_payload(payload);
    let result = provider(&store)
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_bad_transport", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("missing account must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn prompt_cache_key_should_become_an_opaque_session_affinity_lookup_key() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                ("prompt_cache_key".to_owned(), json!("raw-prompt-cache-key")),
            ]),
        )
        .expect("OpenAI payload"),
    );

    let stream = provider_with_affinity(&store, Arc::clone(&affinity))
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_affinity_key", CancellationToken::new()),
        )
        .await
        .expect("prepare provider stream");
    drop(stream);

    let keys = affinity.lookup_keys();
    assert_eq!(keys.len(), 1);
    assert_ne!(keys[0], "raw-prompt-cache-key");
    assert_eq!(keys[0].len(), 64);
    assert!(keys[0].bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[tokio::test]
async fn completed_response_persists_session_affinity_before_stream_consumer_stops_polling() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_completed_affinity").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_affinity\",\"model\":\"gpt-5.4\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_affinity\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                )),
        )
        .mount(&server)
        .await;
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            Map::from_iter([
                ("model".to_owned(), json!("gpt-5.4")),
                ("input".to_owned(), json!("hello")),
                ("prompt_cache_key".to_owned(), json!("affinity-key")),
            ]),
        )
        .expect("OpenAI payload")
        .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
    );
    let mut stream =
        provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri())
            .execute(
                planned_request("openai", Operation::Generate(generation)),
                context("req_completed_affinity", CancellationToken::new()),
            )
            .await
            .expect("prepare provider stream");

    while let Some(event) = stream.next().await {
        let event = event.expect("provider event");
        if event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)))
        {
            break;
        }
    }
    drop(stream);

    assert_eq!(affinity.binding_count(), 1);
}

#[tokio::test]
async fn disabled_account_diagnostic_uses_upstream_without_persisting_account_state() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_disabled_diagnostic";
    create_account_with_enabled(&store, account_id, false).await;
    let account = store.account(account_id).expect("disabled test account");
    store
        .repository()
        .apply_state(
            &account,
            AccountAvailability::QuotaExhausted,
            Some("quota_exhausted".to_owned()),
            None,
            SystemTime::now(),
        )
        .await
        .expect("seed quota-exhausted state");

    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-active-limit", "codex")
                .insert_header("x-codex-primary-used-percent", "100")
                .insert_header("x-codex-primary-window-minutes", "300")
                .insert_header("x-codex-primary-reset-at", "1900000000")
                .insert_header("x-codex-limit-reached", "true")
                .set_body_string(concat!(
                    "event: response.created\n",
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_disabled_diagnostic\",\"model\":\"gpt-5.4\"}}\n\n",
                    "event: response.completed\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_disabled_diagnostic\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
                )),
        )
        .mount(&server)
        .await;

    let mut stream =
        provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri())
            .execute(
                planned_request("openai", http_generate_operation()),
                diagnostic_context("req_disabled_diagnostic", account_id),
            )
            .await
            .expect("disabled diagnostic should prepare a fixed-account stream");
    let mut completed = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("disabled diagnostic upstream response");
        completed |= event
            .canonical_facts()
            .iter()
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
    }

    assert!(completed);
    let account = store
        .account(account_id)
        .expect("disabled test account after test");
    assert!(!account.enabled());
    assert_eq!(account.availability(), AccountAvailability::QuotaExhausted);
    assert!(!store.has_quota(account_id));
    assert_eq!(affinity.binding_count(), 0);
}

#[test]
fn request_observation_reads_openai_metadata_without_changing_the_operation() {
    let store = Arc::new(MemoryAccountStore::default());
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("input".to_owned(), json!("hello")),
        ]),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([(
        "turn_metadata".to_owned(),
        serde_json::Value::String(
            r#"{"request_kind":"review","subagent_kind":"worker"}"#.to_owned(),
        ),
    )]));
    let generation = GenerateRequest::from_protocol_payload(payload);
    let operation = Operation::Generate(generation);

    let observation = provider(&store).request_observation(&operation);

    assert_eq!(observation.request_kind.as_deref(), Some("review"));
    assert_eq!(observation.subagent_kind.as_deref(), Some("worker"));
    assert!(!observation.compact);
    assert!(matches!(operation, Operation::Generate(_)));
}

#[test]
fn request_observation_preserves_the_raw_reasoning_effort() {
    let store = Arc::new(MemoryAccountStore::default());
    let payload = ProtocolPayload::json_object(
        "openai",
        Map::from_iter([("reasoning".to_owned(), json!({"effort": "future-value"}))]),
    )
    .expect("protocol payload");
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));

    let observation = provider(&store).request_observation(&operation);

    assert_eq!(
        observation.reasoning_effort.as_deref(),
        Some("future-value")
    );
}

#[tokio::test]
async fn provider_compiles_catalog_presentation_for_codex_models() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_presentation").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(OFFICIAL_FIXTURE.to_vec(), "application/json"),
        )
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());

    let capabilities = provider
        .query_model_capabilities()
        .await
        .expect("capabilities");

    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].upstream_model().as_str(), "gpt-5.4");
    let presentation = capabilities[0]
        .presentation()
        .expect("Codex model presentation");
    assert_eq!(presentation.display_name(), Some("GPT-5.4"));
    assert_eq!(
        presentation.description(),
        Some("Frontier agentic coding model.")
    );
    assert_eq!(presentation.supported_reasoning_efforts(), ["low", "high"]);
    assert_eq!(presentation.default_reasoning_effort(), Some("low"));
    assert_eq!(presentation.context_window_tokens(), Some(272_000));
    assert!(presentation.image_input());
    assert!(presentation.agent_tools());
    assert!(presentation.parallel_tool_calls());
    assert!(presentation.search_tool());
    assert!(presentation.image_detail_original());
    assert!(presentation.verbosity());
    assert!(!presentation.hidden());
}
