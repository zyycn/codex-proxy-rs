use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use futures::stream;

use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRegistry, ProviderRequest, ProviderResource, ProviderStream,
    RegistryError, ResourceId, UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, UpstreamSendState};
use gateway_core::error::{IdentifierError, ProviderError, ProviderErrorKind};
use gateway_core::operation::OperationKind;
use gateway_core::routing::{
    InstanceHealth, ModelCapabilities, ProviderInstance, ProviderInstanceId, ProviderKind,
    UpstreamModelId,
};

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
        _instance: &ProviderInstance,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(vec![ProviderModelCapabilities::new(
            UpstreamModelId::new("live-model").expect("model"),
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), 128_000, None),
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
    let instance = ProviderInstance::new(
        ProviderInstanceId::new("inst_openai").expect("instance"),
        ProviderKind::new("openai").expect("provider kind"),
        "https://api.example".to_owned(),
        true,
        InstanceHealth::Healthy,
    );

    let models = futures::executor::block_on(registry.query_model_capabilities(&instance))
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
        ProviderInstanceId::new("inst_openai").expect("valid instance"),
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
