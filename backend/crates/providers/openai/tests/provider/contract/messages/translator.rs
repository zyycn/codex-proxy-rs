//! 转换失败保留受限诊断原因，不改变发送状态或公共错误

use super::*;
use gateway_core::event::{ProtocolWireEvent, ProviderEvent};
use gateway_protocol::anthropic::TranslationError;
use std::error::Error as _;

#[tokio::test]
async fn response_translation_preserves_causes_in_both_directions() {
    for (target, protocol, operation, message) in [
        (
            ApiKeyProtocol::Messages,
            "anthropic",
            generate_with_downstream_headers(),
            "The Messages event type is missing.",
        ),
        (
            ApiKeyProtocol::Responses,
            "openai",
            messages_operation(true),
            "The Responses event type is missing.",
        ),
    ] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        seed(&store, &server, target).await;
        let mut stream = provider(&store)
            .execute(
                planned_request("openai", operation),
                context("req_translation_cause", CancellationToken::new()),
            )
            .await
            .unwrap();
        let wire =
            ProtocolWireEvent::json(protocol, None, json!({"private":"PRIVATE_INPUT"})).unwrap();
        let error = stream
            .translate_native_response(ProviderEvent::wire(wire), false)
            .unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
        assert_eq!(error.send_state(), UpstreamSendState::Sent);
        assert_eq!(
            error.source().unwrap().downcast_ref::<TranslationError>(),
            Some(&TranslationError::InvalidEvent(message))
        );
        assert!(error.error_details().unwrap().contains(message));
        assert!(!format!("{error:?} {error}").contains(message));
        assert!(!error.error_details().unwrap().contains("PRIVATE_INPUT"));
    }
}

#[tokio::test]
async fn request_translation_preserves_the_unsent_validation_cause() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    seed(&store, &server, ApiKeyProtocol::Messages).await;
    let body = json!({"model":"gpt-5.4","input":"hello","tool_choice":{"type":"allowed_tools","mode":"required","tools":[]}});
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone()).unwrap(),
    ));
    let error = provider(&store)
        .execute(
            planned_request("openai", operation),
            context("req_translation_validation", CancellationToken::new()),
        )
        .await
        .err()
        .expect("invalid allowed tools");
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert_eq!(
        error.source().unwrap().downcast_ref::<TranslationError>(),
        Some(&TranslationError::InvalidRequest(
            "Allowed tools must contain at least one tool."
        ))
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
