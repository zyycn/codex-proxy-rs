use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::engine::provider::{Provider as _, ProviderRequest};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId,
    RequestAttemptContext, UpstreamSendState,
};
use gateway_core::error::ProviderErrorKind;
use gateway_core::operation::{
    CompactConversationRequest, ContentPart, GenerateRequest, Message, MessageRole, Operation,
    ProviderOptions,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{
    ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, PublicModelId, RoutingContext,
    RuntimeSnapshot, UpstreamModelId,
};
use provider_openai::CodexProvider;
use provider_openai::credential::{
    CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialQuotaService,
    CodexCredentialSelector,
};
use provider_openai::transport::CodexWebSocketPool;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::{Map, json};

use crate::support::{MemoryAccountStore, TestLeaseCoordinator, account_policy};

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
    let profile = wire_profile();
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
    ));
    let selector = Arc::new(CodexCredentialSelector::new(
        ProviderKind::new("openai").expect("provider"),
        store.repository(),
        Arc::new(TestLeaseCoordinator::default()),
        Arc::clone(&catalog),
        Arc::clone(&quota),
        CodexCookiePolicy::official().expect("cookie policy"),
    ));

    CodexProvider::new(
        selector,
        catalog,
        quota,
        http,
        profile,
        Arc::new(CodexWebSocketPool::default()),
    )
    .expect("official OpenAI provider")
}

fn generate_operation() -> Operation {
    Operation::Generate(
        GenerateRequest::new(vec![
            Message::new(
                MessageRole::User,
                vec![ContentPart::Text("hello".to_owned())],
            )
            .expect("message"),
        ])
        .expect("generate request"),
    )
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
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), 128_000, Some(32_000)),
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
async fn openai_provider_rejects_compaction_as_an_unsupported_operation() {
    let store = Arc::new(MemoryAccountStore::default());
    let Operation::Generate(generation) = generate_operation() else {
        unreachable!("fixture is generate")
    };
    let operation = Operation::CompactConversation(CompactConversationRequest::new(generation));
    let result = provider(&store)
        .execute(
            planned_request("openai", operation),
            context("req_compaction", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("OpenAI compaction must remain unsupported")
    };

    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
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

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[tokio::test]
async fn unknown_openai_transport_is_rejected_before_account_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    let Operation::Generate(mut generation) = generate_operation() else {
        unreachable!("fixture is generate")
    };
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([("transport".to_owned(), json!("unsupported"))]),
        )
        .expect("provider options");
    generation = generation.with_provider_options(options);
    let result = provider(&store)
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_bad_transport", CancellationToken::new()),
        )
        .await;
    let Err(error) = result else {
        panic!("unknown transport must fail")
    };

    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
}

#[test]
fn request_observation_reads_openai_metadata_without_changing_the_operation() {
    let store = Arc::new(MemoryAccountStore::default());
    let Operation::Generate(mut generation) = generate_operation() else {
        unreachable!("fixture is generate")
    };
    let mut options = ProviderOptions::new();
    options
        .insert(
            "openai",
            Map::from_iter([
                ("schema_version".to_owned(), json!(1)),
                (
                    "turn_metadata".to_owned(),
                    json!(r#"{"request_kind":"review","subagent_kind":"worker"}"#),
                ),
            ]),
        )
        .expect("provider options");
    generation = generation.with_provider_options(options);
    let operation = Operation::Generate(generation);

    let observation = provider(&store).request_observation(&operation);

    assert_eq!(observation.request_kind.as_deref(), Some("review"));
    assert_eq!(observation.subagent_kind.as_deref(), Some("worker"));
    assert!(!observation.compact);
    assert!(matches!(operation, Operation::Generate(_)));
}
