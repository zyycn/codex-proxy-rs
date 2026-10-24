//! attempt 中间件在 Core 准入前保持冷流，读取后的错误保留发送事实

use super::*;
use gateway_core::{error::ProviderErrorKind, upstream::UpstreamSendState};

#[derive(Debug, Clone, Copy)]
enum Action {
    RejectBeforeNext,
    FaultBeforeNext,
    RejectAfterRead,
    ReturnAfterRead,
}

#[derive(Debug)]
struct InspectingPlan(Action);

impl MiddlewarePlan for InspectingPlan {
    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let action = self.0;
        Box::pin(async move {
            match action {
                Action::RejectBeforeNext => return Err(MiddlewareError::Rejected),
                Action::FaultBeforeNext => return Err(MiddlewareError::Fault),
                Action::RejectAfterRead | Action::ReturnAfterRead => {}
            }
            let response = next.run(request).await?;
            let (protocol, status, headers, mut body, envelope) = response.into_parts();
            let first = body.next_frame().await?.expect("upstream first frame");
            if matches!(action, Action::RejectAfterRead) {
                return Err(MiddlewareError::Rejected);
            }
            Ok(MiddlewareResponse::new(
                protocol,
                status,
                headers,
                Box::new(InspectedBody {
                    first: Some(first),
                    body,
                }),
            )
            .with_envelope(envelope.expect("host metadata")))
        })
    }
}

struct InspectedBody {
    first: Option<MiddlewareFrame>,
    body: Box<dyn MiddlewareBody>,
}

impl MiddlewareBody for InspectedBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            if let Some(first) = self.first.take() {
                Ok(Some(first))
            } else {
                self.body.next_frame().await
            }
        })
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.body.close()
    }
}

struct DropLease(Arc<AtomicUsize>);

impl Drop for DropLease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn plan(action: Action) -> FrozenMiddlewarePlan {
    FrozenMiddlewarePlan::new(
        Arc::new(InspectingPlan(action)),
        ExtensionSetReference::new(
            ExtensionSetId::new("cold-middleware".to_owned()).unwrap(),
            Arc::new(ReadyGeneration),
        ),
    )
}

fn terminal(polls: Arc<AtomicUsize>, released: Arc<AtomicUsize>) -> ProviderMiddlewareTerminal {
    Box::new(move |_, _, _| {
        Box::pin(async move {
            let metadata = ProviderCallMetadata::new(
                ProviderKind::new("openai").unwrap(),
                UpstreamModelId::new("gpt-test").unwrap(),
                ProviderAccountId::new("acct_middleware").unwrap(),
                UpstreamTransport::new("http_sse").unwrap(),
            );
            let events = stream::once(async move {
                polls.fetch_add(1, Ordering::SeqCst);
                Ok(ProviderEvent::canonical_with_wire(
                    vec![GatewayEvent::Started(ResponseMeta::new(
                        "response-cold",
                        "gpt-test",
                    ))],
                    ProtocolWireEvent::json(
                        "openai",
                        Some("response.created".to_owned()),
                        json!({"type":"response.created"}),
                    )
                    .unwrap(),
                ))
            });
            Ok(ProviderStream::new(metadata, events, DropLease(released)))
        })
    })
}

#[test]
fn inspecting_a_response_waits_for_the_outer_stream_and_keeps_send_facts() {
    futures::executor::block_on(async {
        for action in [Action::RejectAfterRead, Action::ReturnAfterRead] {
            let polls = Arc::new(AtomicUsize::new(0));
            let released = Arc::new(AtomicUsize::new(0));
            let mut stream = execute_attempt_middleware(
                Some(&plan(action)),
                context(),
                operation(),
                ClientTransport::HttpJson,
                None,
                terminal(polls.clone(), released.clone()),
            )
            .await
            .unwrap();
            assert_eq!(polls.load(Ordering::SeqCst), 0);
            assert_eq!(released.load(Ordering::SeqCst), 0);
            let first = stream.next().await.unwrap();
            match action {
                Action::RejectAfterRead => {
                    let error = first.unwrap_err();
                    assert_eq!(error.kind(), ProviderErrorKind::RequestPolicyDenied);
                    assert_eq!(error.send_state(), UpstreamSendState::Sent);
                }
                Action::ReturnAfterRead => {
                    assert!(matches!(
                        first.unwrap().canonical_facts(),
                        [GatewayEvent::Started(_)]
                    ));
                }
                _ => unreachable!(),
            }
            assert_eq!(polls.load(Ordering::SeqCst), 1);
            drop(stream);
            assert_eq!(released.load(Ordering::SeqCst), 1);
        }
    });
}

#[test]
fn rejecting_metadata_drops_the_unactivated_handler_and_account_lease() {
    futures::executor::block_on(async {
        let polls = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let stream = execute_attempt_middleware(
            Some(&plan(Action::RejectAfterRead)),
            context(),
            operation(),
            ClientTransport::HttpJson,
            None,
            terminal(polls.clone(), released.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_middleware"
        );
        drop(stream);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn middleware_rejection_before_next_does_not_wait_for_activation() {
    futures::executor::block_on(async {
        for (action, kind) in [
            (
                Action::RejectBeforeNext,
                ProviderErrorKind::RequestPolicyDenied,
            ),
            (Action::FaultBeforeNext, ProviderErrorKind::Protocol),
        ] {
            let error = execute_attempt_middleware(
                Some(&plan(action)),
                context(),
                operation(),
                ClientTransport::HttpJson,
                None,
                Box::new(|_, _, _| panic!("rejected middleware cannot prepare upstream")),
            )
            .await
            .err()
            .expect("prepare rejection");
            assert_eq!(error.kind(), kind);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        }
    });
}
