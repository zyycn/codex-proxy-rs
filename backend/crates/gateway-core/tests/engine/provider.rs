use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use futures_timer::Delay;

use gateway_core::engine::credential::{
    AccountFeedbackStats, CredentialRevision, ProviderAccountId,
};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRegistry, ProviderRequest, ProviderResource, ProviderStream,
    RegistryError, ResourceId, UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, UpstreamSendState};
use gateway_core::error::{IdentifierError, ProviderError, ProviderErrorKind};
use gateway_core::event::{ContentItem, ContentKind, GatewayEvent, ResponseMeta, TextDelta};
use gateway_core::operation::OperationKind;
use gateway_core::routing::{ModelCapabilities, ProviderKind, UpstreamModelId};

struct NamedProvider(&'static str);

#[async_trait]
impl Provider for NamedProvider {
    fn name(&self) -> &'static str {
        self.0
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(vec![ProviderModelCapabilities::new(
            UpstreamModelId::new("live-model").expect("model"),
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None),
        )])
    }

    async fn execute(
        &self,
        _request: ProviderRequest,
        _context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unavailable,
            UpstreamSendState::NotSent,
        ))
    }
}

#[test]
fn registry_should_reject_duplicate_provider_name() {
    let mut builder = ProviderRegistry::builder();
    builder
        .register(Arc::new(NamedProvider("openai")))
        .expect("first provider is valid");

    let error = builder
        .register(Arc::new(NamedProvider("openai")))
        .expect_err("duplicate provider must fail");

    assert_eq!(
        error,
        RegistryError::Duplicate {
            provider: "openai".to_owned()
        }
    );
}

#[test]
fn registry_should_query_provider_compiled_model_capabilities() {
    let mut builder = ProviderRegistry::builder();
    builder
        .register(Arc::new(NamedProvider("openai")))
        .expect("provider");
    let registry = builder.build();
    let provider = ProviderKind::new("openai").expect("provider kind");
    let models = futures::executor::block_on(registry.query_model_capabilities(&provider))
        .expect("live catalog");

    assert_eq!(models[0].upstream_model().as_str(), "live-model");
    assert!(
        models[0]
            .capabilities()
            .match_requirements(&gateway_core::operation::CapabilityRequirements::new(
                OperationKind::Generate,
            ))
            .is_some()
    );
}

struct DropLease(Arc<AtomicBool>);

impl Drop for DropLease {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn provider_stream_should_release_owned_lease_on_drop() {
    let released = Arc::new(AtomicBool::new(false));
    let metadata = ProviderCallMetadata::new(
        ProviderKind::new("openai").expect("valid provider"),
        UpstreamModelId::new("gpt-5").expect("valid model"),
        ProviderResource::Anonymous(ResourceId::none()),
        UpstreamTransport::new("http_sse").expect("valid transport"),
    );
    let events: EventStream = Box::pin(stream::empty());
    let provider_stream = ProviderStream::new(metadata, events, DropLease(Arc::clone(&released)));

    drop(provider_stream);

    assert!(released.load(Ordering::SeqCst));
}

#[test]
fn provider_stream_should_report_common_account_success_and_first_output() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("valid provider");
    let account = ProviderAccountId::new("acct_stream_success").expect("account");
    let metadata = ProviderCallMetadata::new(
        provider.clone(),
        UpstreamModelId::new("gpt-5").expect("valid model"),
        ProviderResource::Account {
            id: account.clone(),
            revision: CredentialRevision::new(1).expect("revision"),
        },
        UpstreamTransport::new("http_sse").expect("valid transport"),
    );
    let response = ResponseMeta::new("resp_upstream", "gpt-5");
    let events: EventStream = Box::pin(
        stream::iter([
            Ok(GatewayEvent::Started(response.clone()).into()),
            Ok(GatewayEvent::ContentAdded(ContentItem::new(0, ContentKind::Text)).into()),
            Ok(GatewayEvent::TextDelta(TextDelta {
                content_index: 0,
                text: "hello".to_owned(),
            })
            .into()),
            Ok(GatewayEvent::Completed(response).into()),
        ])
        .then(|event| async move {
            Delay::new(std::time::Duration::from_millis(2)).await;
            event
        }),
    );
    let mut provider_stream =
        ProviderStream::new(metadata, events, ()).with_account_feedback(Arc::clone(&feedback));

    futures::executor::block_on(async {
        while let Some(event) = provider_stream.next().await {
            event.expect("valid provider event");
        }
    });

    let (failure_rate, first_output_ms) = feedback.scheduling_signals(&provider, &account);
    assert_eq!(failure_rate, Some(0));
    assert!(first_output_ms.is_some_and(|value| value >= 2));
}

#[test]
fn provider_stream_should_report_sent_failure_but_ignore_not_sent_failure() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("xai").expect("valid provider");
    let sent_account = ProviderAccountId::new("acct_stream_sent").expect("account");
    let not_sent_account = ProviderAccountId::new("acct_stream_not_sent").expect("account");
    for (account, send_state) in [
        (sent_account.clone(), UpstreamSendState::Sent),
        (not_sent_account.clone(), UpstreamSendState::NotSent),
    ] {
        let metadata = ProviderCallMetadata::new(
            provider.clone(),
            UpstreamModelId::new("grok-4.5").expect("valid model"),
            ProviderResource::Account {
                id: account,
                revision: CredentialRevision::new(1).expect("revision"),
            },
            UpstreamTransport::new("http_sse").expect("valid transport"),
        );
        let events: EventStream = Box::pin(stream::iter([Err(ProviderError::new(
            ProviderErrorKind::Transport,
            send_state,
        ))]));
        let mut provider_stream =
            ProviderStream::new(metadata, events, ()).with_account_feedback(Arc::clone(&feedback));
        futures::executor::block_on(async {
            assert!(
                provider_stream
                    .next()
                    .await
                    .is_some_and(|event| event.is_err())
            );
        });
    }

    assert_eq!(
        feedback.scheduling_signals(&provider, &sent_account).0,
        Some(2_000)
    );
    assert_eq!(
        feedback.scheduling_signals(&provider, &not_sent_account),
        (None, None)
    );
}

#[test]
fn resource_id_should_accept_versioned_hmac_pseudonym() {
    let resource = ResourceId::anonymous("rr_hmac_sha256_v1:opaque-digest")
        .expect("versioned pseudonym is valid");

    assert_eq!(resource.as_str(), "rr_hmac_sha256_v1:opaque-digest");
}

#[test]
fn resource_id_should_reserve_none_sentinel_for_constructor() {
    let error = ResourceId::anonymous("__none__")
        .expect_err("sentinel must only be created by ResourceId::none");

    assert_eq!(error, IdentifierError::ReservedPrefix);
}
