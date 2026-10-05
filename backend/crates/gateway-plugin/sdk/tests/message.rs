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
