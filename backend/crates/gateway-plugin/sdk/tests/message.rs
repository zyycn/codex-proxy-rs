//! 验证协议消息的诊断输出不泄漏对端载荷与错误详情

use gateway_plugin_sdk::{Frame, Message};

#[test]
fn diagnostics_do_not_render_peer_payloads_or_error_details() {
    let secret = "sensitive-fixture-value";
    let mut fault =
        gateway_plugin_sdk::PluginFault::new(gateway_plugin_sdk::ErrorCode::Fault, secret);
    fault.details = Some(
        serde_json::json!({"credential":secret,"empty":"","disabled":false,"zero":0,"missing":null}),
    );
    let encoded = serde_json::to_vec(&fault).unwrap();
    let decoded: gateway_plugin_sdk::PluginFault = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, fault);
    let frame = Frame {
        message: Message::Result {
            id: 1,
            result: serde_json::json!({"credential":secret}),
        },
        payload: secret.as_bytes().to_vec(),
    };
    assert!(!format!("{frame:?} {fault:?}").contains(secret));
}

#[cfg(feature = "io")]
#[tokio::test]
async fn malformed_frame_metadata_retains_the_native_json_location() {
    use std::error::Error as _;
    let metadata = br#"{"type": "PRIVATE_UNKNOWN_MESSAGE_KIND"}"#;
    let mut wire = Vec::new();
    wire.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
    wire.extend_from_slice(&0_u64.to_be_bytes());
    wire.extend_from_slice(metadata);
    let error = gateway_plugin_sdk::client::read_frame(&mut wire.as_slice())
        .await
        .unwrap_err();
    let source = error
        .source()
        .unwrap()
        .downcast_ref::<serde_json::Error>()
        .unwrap();
    assert_eq!(source.line(), 1);
    assert!(source.column() > 0);
    assert!(source.is_data());
}
