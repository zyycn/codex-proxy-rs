//! 出站插件的外部副作用在错误改写后仍约束 Engine 重放

use super::*;
use gateway_core::engine::extensions::{ExecutionExtensionIndex, ExecutionExtensionPlans};
use gateway_core::engine::middleware::{
    MiddlewareContext, MiddlewareError, MiddlewareMount, MiddlewareNext, MiddlewarePlan,
    MiddlewareRequest, MiddlewareResponse, outbound,
};

#[derive(Debug)]
struct EffectfulOutbound {
    catch_error: bool,
    caught: Arc<AtomicUsize>,
}

impl MiddlewarePlan for EffectfulOutbound {
    fn has_outbound(&self, _context: &MiddlewareContext) -> bool {
        true
    }

    fn handle_outbound(
        &self,
        context: MiddlewareContext,
        _: outbound::Request,
        _: outbound::Next,
    ) -> BoxFuture<'static, Result<outbound::Request, MiddlewareError>> {
        Box::pin(async move {
            // 对应 Runtime 确认受管 HTTP 已发送后的唯一记录入口
            context.execution_effects().unwrap().observe();
            Err(MiddlewareError::Fault)
        })
    }

    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let catch_error = self.catch_error && context.mount() == MiddlewareMount::Attempt;
        if !catch_error {
            return next.run(request);
        }
        let caught = Arc::clone(&self.caught);
        Box::pin(async move {
            let response = next.run(request).await?;
            let (_, _, _, mut body, _) = response.into_parts();
            body.next_frame()
                .await
                .expect_err("outbound preparation fails before a frame");
            body.close().await;
            caught.fetch_add(1, Ordering::SeqCst);
            Err(MiddlewareError::Provider(ProviderError::new(
                ProviderErrorKind::Transport,
                UpstreamSendState::NotSent,
            )))
        })
    }
}

struct PreparingProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for PreparingProvider {
    fn name(&self) -> &'static str {
        "openai"
    }
    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(vec![])
    }

    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let provider = request.candidate().provider().clone();
        let model = request
            .candidate()
            .upstream_model()
            .map(|model| model.as_str().to_owned());
        let account = ProviderAccountId::new("acct_openai").unwrap();
        let pipeline = context
            .outbound_middleware(provider.clone(), model.clone(), account.clone())
            .unwrap();
        let metadata = ProviderCallMetadata::new(
            provider.clone(),
            request.candidate().upstream_model().unwrap().clone(),
            account.clone(),
            UpstreamTransport::new("http").unwrap(),
        );
        context
            .execute_middleware(
                request.operation().clone(),
                provider,
                model,
                account,
                None,
                Box::new(move |_, _, _| {
                    Box::pin(async move {
                        let events = futures::stream::once(async move {
                            Err::<ProviderEvent, _>(
                                pipeline
                                    .prepare(outbound::Request {
                                        target: outbound::Target::Http {
                                            method: http::Method::POST,
                                            uri: "https://example.test/".parse().unwrap(),
                                        },
                                        headers: Vec::new(),
                                        body: bytes::Bytes::from_static(b"request"),
                                    })
                                    .await
                                    .unwrap_err(),
                            )
                        });
                        Ok(ProviderStream::new(metadata, events, ()))
                    })
                }),
            )
            .await
    }
}

struct Fixture {
    service: DefaultExecutionService,
    provider: Arc<PreparingProvider>,
    observer: Arc<RecordingRequestObserver>,
    caught: Arc<AtomicUsize>,
    _extensions: Arc<ExecutionExtensionPlans>,
}

fn fixture(catch_error: bool) -> Fixture {
    let provider = Arc::new(PreparingProvider {
        calls: AtomicUsize::new(0),
    });
    let generation = super::super::extensions::reference("outbound-effect");
    let extension_index = ExecutionExtensionIndex::default();
    let caught = Arc::new(AtomicUsize::new(0));
    let extension_owner = extension_index
        .register(
            generation.id().clone(),
            Arc::new(ExecutionExtensionPlans {
                middleware: Some(Arc::new(EffectfulOutbound {
                    catch_error,
                    caught: Arc::clone(&caught),
                })),
                upstream_adapters: None,
            }),
        )
        .unwrap();
    let observer_index = RequestObserverExtensionIndex::default();
    let observer = Arc::new(RecordingRequestObserver::default());
    let _observer_owner = observer_index
        .register(generation.id().clone(), observer.clone())
        .unwrap();
    let providers = ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap();
    let service = DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(fallback_observation_snapshot(generation)),
        Arc::new(TrackingExecutionStore::default()),
        providers,
        Arc::new(Admissions::default()),
        Arc::new(UnusedContinuation),
        Arc::new(RecordingClientApiKeyUsage::default()),
        Arc::new(crate::RecordingDiagnostics::default()),
    )
    .with_execution_extensions(extension_index)
    .with_request_observers(observer_index);
    Fixture {
        service,
        provider,
        observer,
        caught,
        _extensions: extension_owner,
    }
}

#[test]
fn outbound_external_effect_is_preserved_when_preparation_fails() {
    block_on(async {
        let fixture = fixture(false);
        let mut started = fixture
            .service
            .start(request(&fixture.service, ClientTransport::HttpJson))
            .await
            .unwrap();
        assert!(
            matches!(started.session.collect_uncommitted().await, Err(EngineError::Provider(error)) if error.kind() == ProviderErrorKind::Protocol)
        );
        started.session.detach_finalize().await;
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
        let observations = fixture.observer.observations.lock().unwrap();
        assert_eq!(observations[0].send_state(), UpstreamSendState::Ambiguous);
        assert_eq!(observations[0].attempt_count(), 1);
    });
}

#[test]
fn attempt_cannot_hide_outbound_external_effect_with_retryable_not_sent_error() {
    block_on(async {
        let fixture = fixture(true);
        let mut started = fixture
            .service
            .start(request(&fixture.service, ClientTransport::HttpJson))
            .await
            .unwrap();
        assert!(
            matches!(started.session.collect_uncommitted().await, Err(EngineError::Provider(error)) if error.kind() == ProviderErrorKind::Transport && !error.retry_is_prohibited())
        );
        started.session.detach_finalize().await;
        assert_eq!(fixture.caught.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
        let observations = fixture.observer.observations.lock().unwrap();
        assert_eq!(observations[0].send_state(), UpstreamSendState::Ambiguous);
        assert_eq!(observations[0].attempt_count(), 1);
    });
}
