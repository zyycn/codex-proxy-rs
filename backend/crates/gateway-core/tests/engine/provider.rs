use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use futures_timer::Delay;

use gateway_core::account::{AccountFeedbackStats, ProviderAccountId};
use gateway_core::engine::AttemptContext;
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRegistry, ProviderRequest, ProviderStream, RegistryError,
};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::{
    ContentItem, ContentKind, GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta,
    TextDelta,
};
use gateway_core::operation::OperationKind;
use gateway_core::routing::{ModelCapabilities, ProviderKind, UpstreamModelId};
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
use serde_json::json;

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
        ProviderAccountId::new("acct_drop").expect("account"),
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
        account.clone(),
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
fn provider_stream_should_not_sequence_gate_canonical_observation_attached_to_wire() {
    let metadata = ProviderCallMetadata::new(
        ProviderKind::new("openai").expect("valid provider"),
        UpstreamModelId::new("gpt-5").expect("valid model"),
        ProviderAccountId::new("acct_wire").expect("account"),
        UpstreamTransport::new("http_sse").expect("valid transport"),
    );
    let wire = ProtocolWireEvent::json(
        "openai",
        Some("response.output_text.delta".to_owned()),
        json!({"type":"response.output_text.delta","delta":"hello"}),
    )
    .expect("wire event");
    let events: EventStream = Box::pin(stream::iter([Ok(ProviderEvent::canonical_with_wire(
        vec![GatewayEvent::TextDelta(TextDelta {
            content_index: 0,
            text: "hello".to_owned(),
        })],
        wire,
    ))]));
    let mut provider_stream = ProviderStream::new(metadata, events, ());

    let delivered = futures::executor::block_on(provider_stream.next())
        .expect("wire-backed event")
        .expect("wire-backed observation must be deliverable");

    assert!(matches!(
        delivered.canonical_facts(),
        [GatewayEvent::TextDelta(delta)] if delta.text == "hello"
    ));
    assert!(futures::executor::block_on(provider_stream.next()).is_none());
}

#[test]
fn provider_stream_should_report_confirmed_sent_failure_but_ignore_unconfirmed_failure() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("xai").expect("valid provider");
    let sent_account = ProviderAccountId::new("acct_stream_sent").expect("account");
    let ambiguous_account = ProviderAccountId::new("acct_stream_ambiguous").expect("account");
    let not_sent_account = ProviderAccountId::new("acct_stream_not_sent").expect("account");
    for (account, send_state) in [
        (sent_account.clone(), UpstreamSendState::Sent),
        (ambiguous_account.clone(), UpstreamSendState::Ambiguous),
        (not_sent_account.clone(), UpstreamSendState::NotSent),
    ] {
        let metadata = ProviderCallMetadata::new(
            provider.clone(),
            UpstreamModelId::new("grok-4.5").expect("valid model"),
            account,
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
        feedback.scheduling_signals(&provider, &ambiguous_account),
        (None, None)
    );
    assert_eq!(
        feedback.scheduling_signals(&provider, &not_sent_account),
        (None, None)
    );
}

#[test]
fn provider_stream_should_ignore_repeated_failures_rejected_by_the_filter() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("valid provider");
    let account = ProviderAccountId::new("acct_client_config_error").expect("account");

    for _ in 0..100 {
        let metadata = ProviderCallMetadata::new(
            provider.clone(),
            UpstreamModelId::new("gpt-5").expect("valid model"),
            account.clone(),
            UpstreamTransport::new("http_sse").expect("valid transport"),
        );
        let events: EventStream = Box::pin(stream::iter([Err(ProviderError::new(
            ProviderErrorKind::InvalidRequest,
            UpstreamSendState::Sent,
        ))]));
        let mut provider_stream = ProviderStream::new(metadata, events, ())
            .with_filtered_account_feedback(Arc::clone(&feedback), |_| false);

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
        feedback.scheduling_signals(&provider, &account),
        (None, None)
    );
}

#[test]
fn provider_stream_should_score_only_the_final_same_account_failure() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("valid provider");
    let account = ProviderAccountId::new("acct_same_account_failure").expect("account");

    for retry_index in 1..=3 {
        let metadata = ProviderCallMetadata::new(
            provider.clone(),
            UpstreamModelId::new("gpt-5").expect("valid model"),
            account.clone(),
            UpstreamTransport::new("websocket").expect("valid transport"),
        );
        let error = ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Sent)
            .with_pre_delivery_transport_retry(
                NonZeroU32::new(retry_index).expect("positive retry index"),
                Duration::ZERO,
            );
        let events: EventStream = Box::pin(stream::iter([Err(error)]));
        let mut provider_stream = ProviderStream::new(metadata, events, ())
            .with_filtered_account_feedback(Arc::clone(&feedback), |_| true);

        futures::executor::block_on(async {
            assert!(
                provider_stream
                    .next()
                    .await
                    .is_some_and(|event| event.is_err())
            );
        });
    }

    let metadata = ProviderCallMetadata::new(
        provider.clone(),
        UpstreamModelId::new("gpt-5").expect("valid model"),
        account.clone(),
        UpstreamTransport::new("websocket").expect("valid transport"),
    );
    let events: EventStream = Box::pin(stream::iter([Err(ProviderError::new(
        ProviderErrorKind::Transport,
        UpstreamSendState::Sent,
    ))]));
    let mut provider_stream = ProviderStream::new(metadata, events, ())
        .with_filtered_account_feedback(Arc::clone(&feedback), |_| true);
    futures::executor::block_on(async {
        assert!(
            provider_stream
                .next()
                .await
                .is_some_and(|event| event.is_err())
        );
    });

    assert_eq!(
        feedback.scheduling_signals(&provider, &account).0,
        Some(2_000)
    );
}

#[test]
fn provider_stream_should_report_one_success_after_same_account_retry() {
    let feedback = Arc::new(AccountFeedbackStats::default());
    let provider = ProviderKind::new("openai").expect("valid provider");
    let account = ProviderAccountId::new("acct_same_account_success").expect("account");
    let retry_metadata = ProviderCallMetadata::new(
        provider.clone(),
        UpstreamModelId::new("gpt-5").expect("valid model"),
        account.clone(),
        UpstreamTransport::new("websocket").expect("valid transport"),
    );
    let retry_error = ProviderError::new(ProviderErrorKind::Transport, UpstreamSendState::Sent)
        .with_pre_delivery_transport_fallback();
    let retry_events: EventStream = Box::pin(stream::iter([Err(retry_error)]));
    let mut retry_stream = ProviderStream::new(retry_metadata, retry_events, ())
        .with_filtered_account_feedback(Arc::clone(&feedback), |_| true);
    futures::executor::block_on(async {
        assert!(
            retry_stream
                .next()
                .await
                .is_some_and(|event| event.is_err())
        );
    });

    let success_metadata = ProviderCallMetadata::new(
        provider.clone(),
        UpstreamModelId::new("gpt-5").expect("valid model"),
        account.clone(),
        UpstreamTransport::new("http_sse").expect("valid transport"),
    );
    let success_events: EventStream = Box::pin(stream::empty());
    let mut success_stream = ProviderStream::new(success_metadata, success_events, ())
        .with_filtered_account_feedback(Arc::clone(&feedback), |_| true);
    futures::executor::block_on(async {
        assert!(success_stream.next().await.is_none());
    });

    assert_eq!(feedback.scheduling_signals(&provider, &account).0, Some(0));
}
