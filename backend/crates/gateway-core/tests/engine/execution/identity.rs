//! 身份、冻结权限与查询不建立模型执行图；实际执行才检查调用链并占用执行资源

use super::*;
use gateway_core::engine::{
    extensions::{ExtensionCallScope, InvocationOrigin},
    nested::{BoundModelExecutionRequest, NestedModelExecutionPort},
};
use gateway_core::lifecycle::CancellationToken;

#[test]
fn identity_queries_keep_frozen_permissions_without_using_execution_resources() {
    block_on(async {
        let snapshots = RuntimeSnapshotHandle::new(start_snapshot());
        let store = Arc::new(TrackingExecutionStore::default());
        let usage = Arc::new(RecordingClientApiKeyUsage::default());
        let service = DefaultExecutionService::new(
            snapshots.clone(),
            store.clone(),
            ProviderRegistry::new([
                Arc::new(NativeCatalogProvider { fail: false }) as Arc<dyn Provider>
            ])
            .unwrap(),
            Arc::new(UnusedAdmissions),
            Arc::new(UnusedContinuation),
            usage.clone(),
            Arc::new(crate::RecordingDiagnostics::default()),
        );
        let settings = service.request_settings().unwrap();
        let key = ClientApiKeyId::new("key_start_test").unwrap();
        let client =
            ClientIdentityPort::resolve_client(&service, key.clone(), Some(settings.clone()))
                .await
                .unwrap();
        snapshots.publish(start_snapshot_with_policy(
            2,
            false,
            RateLimits::unlimited(),
            FastMode::Disabled,
        ));
        assert!(
            client
                .policy()
                .account_scope()
                .allows(&ProviderAccountId::new("acct_start").unwrap(),)
        );
        assert!(
            !client
                .policy()
                .account_scope()
                .allows(&ProviderAccountId::new("acct_missing").unwrap(),)
        );
        let models = NestedModelExecutionPort::models(
            &service,
            client.clone(),
            CancellationToken::default(),
            "codex".into(),
            "0.154.0".into(),
        )
        .await
        .unwrap();
        assert_eq!(models, vec![PublicModelId::new("gpt-start").unwrap()]);
        let frozen = ClientIdentityPort::resolve_client(&service, key.clone(), Some(settings))
            .await
            .unwrap();
        assert_eq!(frozen.snapshot().revision().get(), 1);
        let current = ClientIdentityPort::resolve_client(&service, key, None).await;
        assert!(matches!(current, Err(error) if error.kind() == GatewayErrorKind::Unauthorized));

        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let cancelled = NestedModelExecutionPort::models(
            &service,
            client,
            cancellation,
            "codex".into(),
            "0.154.0".into(),
        )
        .await;
        assert!(matches!(cancelled, Err(error) if error.kind() == GatewayErrorKind::Cancelled));
        assert!(usage.recorded().is_empty());
        assert!(!store.touched.load(Ordering::SeqCst));
    });
}

#[test]
fn execution_scope_limits_apply_only_when_starting_a_model() {
    block_on(async {
        let store = Arc::new(TrackingExecutionStore::default());
        let usage = Arc::new(RecordingClientApiKeyUsage::default());
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(start_snapshot()),
            store.clone(),
            ProviderRegistry::new([
                Arc::new(NativeCatalogProvider { fail: false }) as Arc<dyn Provider>
            ])
            .unwrap(),
            Arc::new(UnusedAdmissions),
            Arc::new(UnusedContinuation),
            usage.clone(),
            Arc::new(crate::RecordingDiagnostics::default()),
        );
        let repeated = ExtensionCallScope::default()
            .extending("entry".into())
            .unwrap();
        let exhausted = (0..ExtensionCallScope::MAXIMUM_DEPTH)
            .fold(ExtensionCallScope::default(), |scope, index| {
                scope.extending(format!("ancestor-{index}")).unwrap()
            });
        for extension_scope in [repeated, exhausted] {
            let client = ClientIdentityPort::resolve_client(
                &service,
                ClientApiKeyId::new("key_start_test").unwrap(),
                None,
            )
            .await
            .unwrap();
            let result = NestedModelExecutionPort::start_bound(
                &service,
                BoundModelExecutionRequest {
                    client: client.clone(),
                    initiating_plugin_instance_id: "entry".into(),
                    cancellation: CancellationToken::default(),
                    extension_scope,
                    public_model: PublicModelId::new("gpt-start").unwrap(),
                    operation: start_operation(),
                    metadata: ExecutionRequestMetadata {
                        protocol: "openai".into(),
                        endpoint: "host.model".into(),
                        transport: ClientTransport::InternalPlugin,
                        stream: false,
                        client_ip: None,
                        user_agent: None,
                        previous_response_id: None,
                    },
                    provider: None,
                    account: None,
                },
            )
            .await;
            assert!(matches!(result, Err(error) if error.kind() == GatewayErrorKind::PolicyDenied));
            let models = NestedModelExecutionPort::models(
                &service,
                client,
                CancellationToken::default(),
                "codex".into(),
                "0.154.0".into(),
            )
            .await
            .unwrap();
            assert_eq!(models, vec![PublicModelId::new("gpt-start").unwrap()]);
        }
        assert!(usage.recorded().is_empty());
        assert!(!store.touched.load(Ordering::SeqCst));
    });
}

struct ParentAwareAuthentication {
    calls: AtomicUsize,
    mapped: bool,
}

impl FrontendAuthenticationPlan for ParentAwareAuthentication {
    fn authenticate<'a>(
        &'a self,
        request: &'a ClientAuthenticationRequest,
    ) -> BoxFuture<'a, Result<FrontendAuthenticationDecision, FrontendAuthenticationError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let origin = request.invocation().expect("parent invocation");
            assert_eq!(origin.request_id, "http-request");
            assert_eq!(origin.call_id, "http-call");
            assert!(origin.extensions.contains("authentication-plugin"));
            assert!(origin.effects.is_some());
            assert!(!origin.cancellation.is_cancelled());
            assert_eq!(request.request_context().unwrap().uri, "/plugin/proxy");
            assert!(request.settings().is_some());
            Ok(FrontendAuthenticationDecision::Authenticated {
                principal: "external-principal".into(),
            })
        })
    }

    fn client_key_id(&self, principal: &str) -> Option<ClientApiKeyId> {
        assert_eq!(principal, "external-principal");
        self.mapped
            .then(|| ClientApiKeyId::new("key_start_test").unwrap())
    }

    fn exclusive(&self) -> bool {
        true
    }
}

#[test]
fn verification_keeps_parent_origin_and_requires_the_frontend_principal_mapping() {
    block_on(async {
        for mapped in [true, false] {
            let authentication = FrontendAuthenticationExtensionIndex::default();
            let plan = Arc::new(ParentAwareAuthentication {
                calls: AtomicUsize::new(0),
                mapped,
            });
            let snapshot = start_snapshot().with_extensions(Some(authentication_generation(
                &authentication,
                "identity-verification",
                plan.clone(),
            )));
            let store = Arc::new(TrackingExecutionStore::default());
            let usage = Arc::new(RecordingClientApiKeyUsage::default());
            let service = DefaultExecutionService::new(
                RuntimeSnapshotHandle::new(snapshot),
                store.clone(),
                ProviderRegistry::default(),
                Arc::new(UnusedAdmissions),
                Arc::new(UnusedContinuation),
                usage.clone(),
                Arc::new(crate::RecordingDiagnostics::default()),
            )
            .with_frontend_authentication(authentication);
            let settings = service.request_settings().unwrap();
            let origin = InvocationOrigin {
                request_id: "http-request".into(),
                call_id: "http-call".into(),
                cancellation: CancellationToken::default(),
                extensions: plugin_origin_scope()
                    .extending("authentication-plugin".into())
                    .unwrap(),
                plan: None,
                effects: Some(Arc::new(
                    gateway_core::engine::nested::ExecutionEffects::default(),
                )),
            };
            let mut cancelled_origin = origin.clone();
            cancelled_origin.cancellation = CancellationToken::default();
            cancelled_origin.cancellation.cancel();
            let cancelled = ClientIdentityPort::verify_client(
                &service,
                ClientAuthenticationRequest::new("External fixture")
                    .unwrap()
                    .with_invocation(cancelled_origin)
                    .with_settings(settings.clone()),
            )
            .await;
            assert!(matches!(cancelled, Err(error) if error.kind() == GatewayErrorKind::Cancelled));
            assert_eq!(plan.calls.load(Ordering::SeqCst), 0);
            let result = ClientIdentityPort::verify_client(
                &service,
                ClientAuthenticationRequest::new("External fixture")
                    .unwrap()
                    .with_invocation(origin)
                    .with_settings(settings.clone()),
            )
            .await;
            if mapped {
                let client = result.unwrap();
                assert_eq!(client.policy().key_id().as_str(), "key_start_test");
                assert_eq!(client.request_settings(), Some(&settings));
            } else {
                assert!(
                    matches!(result, Err(error) if error.kind() == GatewayErrorKind::Unauthorized)
                );
            }
            assert_eq!(plan.calls.load(Ordering::SeqCst), 1);
            assert!(usage.recorded().is_empty());
            assert!(!store.touched.load(Ordering::SeqCst));
        }
    });
}
