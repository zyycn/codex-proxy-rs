//! 出站准备处理器经真实 SDK 会话传递二进制正文并等待外层返回

use super::*;
use gateway_plugin_sdk::call::middleware::{
    HANDLE_METHOD, MiddlewareHeader, NEXT_METHOD, outbound,
};
use gateway_plugin_sdk::client::{MiddlewareCall, MiddlewareResult, OutboundRequest};

#[tokio::test]
async fn outbound_builder_preserves_binary_payload_and_outer_changes_after_next() {
    let manifest = gateway_plugin_sdk::Manifest::from_author_slice(&author_manifest(
        json!({"middleware":{"version":6,"stages":["outbound"]}}),
        json!([]),
    ))
    .unwrap();
    let contributes = manifest.contributes.clone();
    let plugin = PluginBuilder::from_manifest(manifest)
        .unwrap()
        .middleware(|call: MiddlewareCall| async move {
            let MiddlewareCall::Outbound(call) = call else {
                return call.forward().await;
            };
            assert_eq!(call.metadata.account_id, "actual-account");
            assert_eq!(call.metadata.attempt_index, 3);
            let mut request = call.request;
            request.body.push(0x80);
            let mut request = call.next.run(request).await?;
            request.body.push(0xff);
            Ok(MiddlewareResult::from(request))
        })
        .unwrap()
        .build()
        .unwrap();
    let (mut host, task) = start_session(plugin, contributes).await;
    let request = outbound::RequestHead {
        target: outbound::Target::Http {
            method: "POST".into(),
            uri: "https://example.test/native?token=private".into(),
        },
        headers: vec![
            MiddlewareHeader {
                name: "x-raw".into(),
                value: vec![0x80],
            },
            MiddlewareHeader {
                name: "x-raw".into(),
                value: b"second".to_vec(),
            },
        ],
    };
    let call = outbound::CallHead {
        request: request.clone(),
        metadata: outbound::Metadata {
            request_id: "request".into(),
            client_key_id: "key".into(),
            account_group_ids: vec!["group".into()],
            provider: "openai".into(),
            model: Some("model".into()),
            account_id: "actual-account".into(),
            attempt_index: 3,
        },
    };
    send_call(
        &mut host,
        1,
        HANDLE_METHOD,
        Stage::Outbound,
        serde_json::to_value(call).unwrap(),
        vec![0, 255, 0],
    )
    .await;
    let callback = receive(&mut host).await;
    let Message::Callback {
        id,
        parent_id,
        method,
        params,
    } = callback.message
    else {
        panic!("expected next callback")
    };
    assert_eq!((parent_id, method.as_str()), (1, NEXT_METHOD));
    assert_eq!(
        serde_json::from_value::<outbound::RequestHead>(params).unwrap(),
        request
    );
    assert_eq!(callback.payload, vec![0, 255, 0, 128]);
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id,
                result: serde_json::to_value(&request).unwrap(),
            },
            payload: callback.payload,
        },
    )
    .await
    .unwrap();
    let (head, body) = unwrap_result(receive(&mut host).await, 1);
    assert_eq!(
        serde_json::from_value::<outbound::RequestHead>(head).unwrap(),
        request
    );
    assert_eq!(body, vec![0, 255, 0, 128, 255]);
    let debug = format!(
        "{:?}",
        OutboundRequest {
            head: request,
            body
        }
    );
    assert!(!debug.contains("private"));
    assert!(!debug.contains("second"));
    assert!(!debug.contains("255"));
    shutdown(&mut host, task).await;
}
