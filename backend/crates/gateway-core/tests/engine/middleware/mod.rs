//! 请求中间件的改写、交付状态与发布代次持有测试

mod cold;
mod outbound;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{StreamExt, stream};
use gateway_core::account::ProviderAccountId;
use gateway_core::account::scope::AccountGroupId;
use gateway_core::engine::ModelRequestId;
use gateway_core::engine::execution::ClientTransport;
use gateway_core::engine::extensions::{
    ExecutionExtensionIndex, ExecutionExtensionPlans, ExtensionCallScope,
};
use gateway_core::engine::middleware::{
    FrozenMiddlewarePlan, MiddlewareAuthority, MiddlewareBody, MiddlewareContext, MiddlewareError,
    MiddlewareFrame, MiddlewareHeader, MiddlewareMount, MiddlewareNext, MiddlewarePlan,
    MiddlewareRequest, MiddlewareResponse, MiddlewareTarget,
};
use gateway_core::engine::provider::{
    ProviderCallMetadata, ProviderMiddlewareTerminal, ProviderStream, execute_attempt_middleware,
};
use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta};
use gateway_core::identity::ProviderKind;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{GenerateRequest, Operation, OperationKind, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::UpstreamModelId;
use gateway_core::routing::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference};
use gateway_core::upstream::UpstreamTransport;
use serde_json::{Value, json};

#[derive(Debug)]
struct RewritingPlan {
    calls: Arc<AtomicUsize>,
}

impl MiddlewarePlan for RewritingPlan {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Attempt);
        assert_eq!(context.attempt_index(), NonZeroU32::new(1));
        assert_eq!(context.operation(), Some(OperationKind::Generate));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let (protocol, headers, body) = request.into_parts();
            let mut body: Value =
                serde_json::from_slice(&body).map_err(|_| MiddlewareError::Fault)?;
            body["input"] = json!("rewritten by middleware");
            let response = next
                .run(MiddlewareRequest::new(
                    protocol,
                    headers,
                    Bytes::from(serde_json::to_vec(&body).map_err(|_| MiddlewareError::Fault)?),
                ))
                .await?;
            let (protocol, status, headers, body, envelope) = response.into_parts();
            let response = MiddlewareResponse::new(
                protocol,
                status,
                headers,
                Box::new(RewritingBody { inner: body }),
            );
            Ok(match envelope {
                Some(envelope) => response.with_envelope(envelope),
                None => response,
            })
        })
    }
}

struct RewritingBody {
    inner: Box<dyn MiddlewareBody>,
}

impl MiddlewareBody for RewritingBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            let Some(frame) = self.inner.next_frame().await? else {
                return Ok(None);
            };
            let (bytes, framing, terminal, envelope) = frame.into_parts();
            let mut body: Value =
                serde_json::from_slice(&bytes).map_err(|_| MiddlewareError::Fault)?;
            body["middleware"] = json!(true);
            let frame = MiddlewareFrame::new(
                Bytes::from(serde_json::to_vec(&body).map_err(|_| MiddlewareError::Fault)?),
                framing,
                terminal,
            );
            Ok(Some(match envelope {
                Some(envelope) => frame.with_envelope(envelope),
                None => frame,
            }))
        })
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.inner.commit_downstream(client_status_code)
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.inner.record_client_status(client_status_code)
    }

    fn is_finalized(&self) -> bool {
        self.inner.is_finalized()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        let Self { inner } = *self;
        inner.close()
    }
}

#[derive(Debug)]
struct ReadyGeneration;

impl ExtensionSetLease for ReadyGeneration {
    fn is_ready(&self) -> bool {
        true
    }
}

fn context() -> MiddlewareContext {
    MiddlewareContext::new(
        MiddlewareTarget {
            request_id: ModelRequestId::new("req_middleware_owned_next").unwrap(),
            mount: MiddlewareMount::Attempt,
            attempt_index: NonZeroU32::new(1),
            operation: Some(OperationKind::Generate),
            endpoint: "/v1/responses".to_owned(),
            transport: ClientTransport::HttpJson,
            provider: Some(ProviderKind::new("openai").unwrap()),
            model: Some("gpt-test".to_owned()),
            account_id: Some(ProviderAccountId::new("acct_middleware").unwrap()),
        },
        MiddlewareAuthority {
            client_key_id: ClientApiKeyId::new("key_middleware").unwrap(),
            account_group_ids: Arc::<[AccountGroupId]>::from([]),
            cancellation: CancellationToken::new(),
            deadline: (SystemTime::now() + Duration::from_secs(5)).into(),
            extension_scope: ExtensionCallScope::default(),
            execution_effects: None,
        },
    )
}

fn operation() -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-test","input":"original"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ))
}

fn body_operation(body: &Value) -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone()).unwrap(),
    ))
}

#[test]
fn capability_declaration_preserves_original_semantics_and_recomputes_upstream_needs() {
    use gateway_core::{engine::middleware::MiddlewareCapabilityDeclaration, operation::Feature};
    let original = json!({"model":"gpt-test","input":"answer", "tools":[{"type":"function"}],
        "text":{"format":{"type":"json_schema","schema":{"type":"object"}}}});
    let mut converted = original.clone();
    converted.as_object_mut().unwrap().remove("text");
    converted["input"] = json!(
        "Return an object matching the supplied schema; the middleware validates the response."
    );
    let request = MiddlewareRequest::new(
        "openai",
        vec![],
        serde_json::to_vec(&original).unwrap().into(),
    )
    .replace_parts(
        "openai".into(),
        vec![],
        serde_json::to_vec(&converted).unwrap().into(),
        Some(MiddlewareCapabilityDeclaration {
            handled: [Feature::JsonSchema].into(),
            required: [Feature::Reasoning].into(),
        }),
    )
    .unwrap();
    let operation = request
        .apply_capabilities(body_operation(&converted))
        .unwrap();
    assert_eq!(
        operation.capability_requirements().features(),
        &[Feature::Tools, Feature::Reasoning].into()
    );
    assert_eq!(
        request.body().as_ref(),
        serde_json::to_vec(&converted).unwrap()
    );

    // 后续普通正文修改也不能删掉未承担的原始语义，或藏起实际新增的需求
    converted.as_object_mut().unwrap().remove("tools");
    converted["input"] = json!([{"type":"input_image","image_url":"synthetic"}]);
    let request = request
        .replace_parts(
            "openai".into(),
            vec![],
            serde_json::to_vec(&converted).unwrap().into(),
            None,
        )
        .unwrap();
    let operation = request
        .apply_capabilities(body_operation(&converted))
        .unwrap();
    assert_eq!(
        operation.capability_requirements().features(),
        &[Feature::Tools, Feature::Reasoning, Feature::Vision].into()
    );
    let changed = operation
        .replace_middleware_wire(
            "openai",
            serde_json::to_vec(&json!({"input":"answer","text":{"format":{"type":"json_schema"}}}))
                .unwrap()
                .into(),
        )
        .unwrap();
    assert!(
        changed
            .capability_requirements()
            .features()
            .contains(&Feature::JsonSchema)
    );
}

#[test]
fn capability_declaration_cannot_erase_remaining_or_invented_features_or_native_continuation() {
    use gateway_core::{engine::middleware::MiddlewareCapabilityDeclaration, operation::Feature};
    let source = json!({"tools":[{"type":"function"}],"previous_response_id":"resp-original"});
    let request = MiddlewareRequest::new(
        "openai",
        vec![],
        serde_json::to_vec(&source).unwrap().into(),
    );
    for (handled, body) in [
        (Feature::Tools, source),
        (Feature::JsonSchema, json!({"input":"removed"})),
        (Feature::NativeContinuation, json!({"input":"removed"})),
    ] {
        assert!(
            request
                .clone()
                .replace_parts(
                    "openai".into(),
                    vec![],
                    serde_json::to_vec(&body).unwrap().into(),
                    Some(MiddlewareCapabilityDeclaration {
                        handled: [handled].into(),
                        required: Default::default()
                    })
                )
                .is_err()
        );
    }
    let converted = json!({"input":"tools converted"});
    let request = request
        .replace_parts(
            "openai".into(),
            vec![],
            serde_json::to_vec(&converted).unwrap().into(),
            Some(MiddlewareCapabilityDeclaration {
                handled: [Feature::Tools].into(),
                required: Default::default(),
            }),
        )
        .unwrap();
    assert!(
        request
            .apply_capabilities(body_operation(&converted))
            .unwrap()
            .capability_requirements()
            .features()
            .contains(&Feature::NativeContinuation)
    );
}

#[test]
fn attempt_middleware_consumes_owned_next_once_and_preserves_host_envelopes() {
    futures::executor::block_on(async {
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let terminal_calls = Arc::new(AtomicUsize::new(0));
        let index = ExecutionExtensionIndex::default();
        let id = ExtensionSetId::new("middleware-generation".to_owned()).unwrap();
        let reference = ExtensionSetReference::new(id.clone(), Arc::new(ReadyGeneration));
        let owner = index
            .register(
                id.clone(),
                Arc::new(ExecutionExtensionPlans {
                    middleware: Some(Arc::new(RewritingPlan {
                        calls: Arc::clone(&plan_calls),
                    })),
                    upstream_adapters: None,
                }),
            )
            .unwrap();
        assert!(
            index
                .register(
                    id,
                    Arc::new(ExecutionExtensionPlans {
                        middleware: Some(Arc::new(RewritingPlan {
                            calls: Arc::new(AtomicUsize::new(0)),
                        })),
                        upstream_adapters: None
                    }),
                )
                .is_err(),
            "同一发布代次不能静默替换中间件计划",
        );
        let plan = index.middleware(&reference).unwrap();

        let terminal: ProviderMiddlewareTerminal = {
            let terminal_calls = Arc::clone(&terminal_calls);
            Box::new(move |operation, headers, _| {
                terminal_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    assert!(headers.is_empty());
                    let Operation::Generate(request) = operation else {
                        panic!("middleware fixture expects generate operation")
                    };
                    assert_eq!(
                        request.protocol_payload().body()["input"],
                        "rewritten by middleware",
                    );
                    let response = ResponseMeta::new("response-middleware", "gpt-test");
                    let event = |kind: &str, fact| {
                        ProviderEvent::canonical_with_wire(
                            vec![fact],
                            ProtocolWireEvent::json(
                                "openai",
                                Some(kind.to_owned()),
                                json!({"type":kind}),
                            )
                            .unwrap(),
                        )
                    };
                    let metadata = ProviderCallMetadata::new(
                        ProviderKind::new("openai").unwrap(),
                        UpstreamModelId::new("gpt-test").unwrap(),
                        ProviderAccountId::new("acct_middleware").unwrap(),
                        UpstreamTransport::new("http_json").unwrap(),
                    );
                    Ok(ProviderStream::new(
                        metadata,
                        stream::iter([
                            Ok(event(
                                "response.created",
                                GatewayEvent::Started(response.clone()),
                            )),
                            Ok(event(
                                "response.completed",
                                GatewayEvent::Completed(response),
                            )),
                        ]),
                        (),
                    ))
                })
            })
        };

        let mut stream = execute_attempt_middleware(
            Some(&plan),
            context(),
            operation(),
            ClientTransport::HttpJson,
            None,
            terminal,
        )
        .await
        .unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert_eq!(plan_calls.load(Ordering::SeqCst), 1);
        assert_eq!(terminal_calls.load(Ordering::SeqCst), 1);
        assert_eq!(stream.metadata().provider().as_str(), "openai");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| {
            event
                .wire_event()
                .and_then(ProtocolWireEvent::raw_json_body)
                .and_then(|body| serde_json::from_slice::<Value>(body).ok())
                .is_some_and(|body| body["middleware"] == json!(true))
        }));
        assert!(matches!(
            events[0].canonical_facts(),
            [GatewayEvent::Started(_)]
        ));
        assert!(matches!(
            events[1].canonical_facts(),
            [GatewayEvent::Completed(_)]
        ));

        drop(owner);
    });
}

#[derive(Debug)]
struct AddingHeaderPlan {
    headers: Vec<MiddlewareHeader>,
}

impl MiddlewarePlan for AddingHeaderPlan {
    fn handle(
        &self,
        _context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let added_headers = self.headers.clone();
        Box::pin(async move {
            let (protocol, mut headers, body) = request.into_parts();
            headers.extend(added_headers);
            next.run(MiddlewareRequest::new(protocol, headers, body))
                .await
        })
    }
}

fn header_plan(generation: &str, headers: Vec<MiddlewareHeader>) -> FrozenMiddlewarePlan {
    FrozenMiddlewarePlan::new(
        Arc::new(AddingHeaderPlan { headers }),
        ExtensionSetReference::new(
            ExtensionSetId::new(generation.to_owned()).unwrap(),
            Arc::new(ReadyGeneration),
        ),
    )
}

fn empty_provider_stream() -> ProviderStream {
    ProviderStream::new(
        ProviderCallMetadata::new(
            ProviderKind::new("openai").unwrap(),
            UpstreamModelId::new("gpt-test").unwrap(),
            ProviderAccountId::new("acct_middleware").unwrap(),
            UpstreamTransport::new("http_json").unwrap(),
        ),
        stream::empty(),
        (),
    )
}

#[test]
fn attempt_passthrough_preserves_parsed_wire_for_every_client_transport() {
    futures::executor::block_on(async {
        for transport in [
            ClientTransport::HttpJson,
            ClientTransport::HttpSse,
            ClientTransport::WebSocket,
        ] {
            let plan = header_plan("middleware-passthrough", Vec::new());
            let payload = json!({"type":"response.completed","response":{"id":"resp_passthrough","status":"completed","output":[]}});
            let wire = ProtocolWireEvent::json(
                "openai",
                Some("response.completed".to_owned()),
                payload.clone(),
            )
            .unwrap();
            let terminal: ProviderMiddlewareTerminal = Box::new(move |_, _, _| {
                Box::pin(async move {
                    Ok(ProviderStream::new(
                        empty_provider_stream().metadata().clone(),
                        stream::iter([Ok(ProviderEvent::wire(wire))]),
                        (),
                    ))
                })
            });
            let mut result = execute_attempt_middleware(
                Some(&plan),
                context(),
                operation(),
                transport,
                None,
                terminal,
            )
            .await
            .unwrap();
            let event = result.next().await.unwrap().unwrap();
            let wire = event.wire_event().unwrap();
            assert!(wire.has_json_data(), "{transport:?} 必须保留客户端终态语义");
            assert_eq!(wire.data(), &payload);
        }
    });
}

#[test]
fn attempt_sse_encoding_keeps_opaque_metadata_from_creating_extra_frames() {
    #[derive(Debug)]
    struct InspectingPlan {
        expected: &'static str,
    }

    impl MiddlewarePlan for InspectingPlan {
        fn handle(
            &self,
            _: MiddlewareContext,
            request: MiddlewareRequest,
            next: MiddlewareNext,
        ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
            let expected = self.expected;
            Box::pin(async move {
                let (protocol, status, headers, mut body, envelope) =
                    next.run(request).await?.into_parts();
                let frame = body.next_frame().await?.expect("provider event");
                assert_eq!(frame.bytes().as_ref(), expected.as_bytes());
                Ok(MiddlewareResponse::new(protocol, status, headers, body)
                    .with_envelope(envelope.expect("provider metadata")))
            })
        }
    }

    futures::executor::block_on(async {
        for (event_type, id, expected) in [
            (
                "future.event",
                "event-1",
                "id: event-1\nretry: 10\nevent: future.event\ndata: {\"value\":1}\n\n",
            ),
            (
                "future.event\ndata: injected\n\n",
                "event-1\r\ndata: injected\r\n\r\n",
                "retry: 10\ndata: {\"value\":1}\n\n",
            ),
            ("", "event\0id", "retry: 10\ndata: {\"value\":1}\n\n"),
        ] {
            let plan = FrozenMiddlewarePlan::new(
                Arc::new(InspectingPlan { expected }),
                ExtensionSetReference::new(
                    ExtensionSetId::new("sse-metadata".to_owned()).unwrap(),
                    Arc::new(ReadyGeneration),
                ),
            );
            let wire = ProtocolWireEvent::json_with_sse_metadata(
                "openai",
                Some(event_type.to_owned()),
                json!({"value":1}),
                Some(id.to_owned()),
                Some(10),
            )
            .unwrap();
            let terminal: ProviderMiddlewareTerminal = Box::new(move |_, _, _| {
                Box::pin(async move {
                    Ok(ProviderStream::new(
                        empty_provider_stream().metadata().clone(),
                        stream::iter([Ok(ProviderEvent::wire(wire))]),
                        (),
                    ))
                })
            });

            execute_attempt_middleware(
                Some(&plan),
                context(),
                operation(),
                ClientTransport::HttpSse,
                None,
                terminal,
            )
            .await
            .unwrap();
        }
    });
}

#[test]
fn attempt_middleware_preserves_multi_value_and_authentication_headers() {
    futures::executor::block_on(async {
        let allowed_calls = Arc::new(AtomicUsize::new(0));
        let allowed_terminal: ProviderMiddlewareTerminal = {
            let allowed_calls = Arc::clone(&allowed_calls);
            Box::new(move |_operation, headers, _| {
                allowed_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    assert_eq!(headers.len(), 2);
                    assert!(
                        headers
                            .iter()
                            .all(|header| header.name() == "x-business-context")
                    );
                    assert_eq!(headers[0].value().as_ref(), b"tenant-public");
                    assert_eq!(headers[1].value().as_ref(), b"trace-public");
                    Ok(empty_provider_stream())
                })
            })
        };
        let allowed = header_plan(
            "middleware-business-header",
            vec![
                MiddlewareHeader::new("x-business-context", Bytes::from_static(b"tenant-public")),
                MiddlewareHeader::new("x-business-context", Bytes::from_static(b"trace-public")),
            ],
        );

        execute_attempt_middleware(
            Some(&allowed),
            context(),
            operation(),
            ClientTransport::HttpJson,
            None,
            allowed_terminal,
        )
        .await
        .unwrap();
        assert_eq!(allowed_calls.load(Ordering::SeqCst), 1);

        let calls = Arc::new(AtomicUsize::new(0));
        let terminal: ProviderMiddlewareTerminal = {
            let calls = calls.clone();
            Box::new(move |_, headers, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    assert_eq!(headers[0].name(), "authorization");
                    assert_eq!(
                        headers[0].value(),
                        &Bytes::from_static(b"Bearer fixture-override")
                    );
                    Ok(empty_provider_stream())
                })
            })
        };
        let plan = header_plan(
            "middleware-authentication-header",
            vec![MiddlewareHeader::new(
                "authorization",
                Bytes::from_static(b"Bearer fixture-override"),
            )],
        );
        execute_attempt_middleware(
            Some(&plan),
            context(),
            operation(),
            ClientTransport::HttpJson,
            None,
            terminal,
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[derive(Debug)]
struct AddingImageTool;

impl MiddlewarePlan for AddingImageTool {
    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async move {
            let (protocol, headers, body) = request.into_parts();
            let mut body: Value = serde_json::from_slice(&body).unwrap();
            body["tools"] = json!([{"type":"image_generation"}]);
            next.run(MiddlewareRequest::new(
                protocol,
                headers,
                serde_json::to_vec(&body).unwrap().into(),
            ))
            .await
        })
    }
}

#[test]
fn attempt_middleware_cannot_add_image_generation_to_a_denied_scope() {
    use gateway_core::account::{AccountSelectionPolicy, RotationStrategy};
    use gateway_core::engine::{AccountAttemptContext, AttemptContext, RequestAttemptContext};
    use gateway_core::error::ProviderErrorKind;
    use gateway_core::routing::{ClientRoutingScope, FrozenAccountScope, RuntimeAccountDirectory};
    use gateway_core::upstream::UpstreamSendState;

    for allowed in [false, true] {
        let scope = FrozenAccountScope::new(
            Arc::new(RuntimeAccountDirectory::new(Default::default())),
            ClientRoutingScope::all_accounts(),
        )
        .with_image_generation(allowed);
        let request = RequestAttemptContext::new(
            ModelRequestId::new("req_image_policy").unwrap(),
            ClientApiKeyId::new("key_image_policy").unwrap(),
        )
        .with_middleware(
            Some(FrozenMiddlewarePlan::new(
                Arc::new(AddingImageTool),
                ExtensionSetReference::new(
                    ExtensionSetId::new("image-policy-test".to_owned()).unwrap(),
                    Arc::new(ReadyGeneration),
                ),
            )),
            Arc::from([]),
            "/v1/responses".into(),
            ClientTransport::HttpJson,
        );
        let attempt = AttemptContext::new(
            request,
            NonZeroU32::new(1).unwrap(),
            SystemTime::now() + Duration::from_secs(5),
            AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::new(1).unwrap()),
            AccountAttemptContext::new(Default::default(), None, None)
                .with_account_scope(Arc::new(scope)),
            None,
            CancellationToken::new(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let result = futures::executor::block_on(attempt.execute_middleware(
            body_operation(&json!({"model":"gpt-test","input":"hello"})),
            ProviderKind::new("openai").unwrap(),
            Some("gpt-test".into()),
            ProviderAccountId::new("acct_middleware").unwrap(),
            None,
            Box::new(move |operation, _, _| {
                Box::pin(async move {
                    assert!(operation.image_generation_requested());
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(empty_provider_stream())
                })
            }),
        ));
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(allowed));
        if allowed {
            assert!(result.is_ok());
        } else {
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("image tool must be denied"),
            };
            assert_eq!(error.kind(), ProviderErrorKind::RequestPolicyDenied);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            assert!(error.retry_is_prohibited());
        }
    }
}
