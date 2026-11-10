//! 模型目录能力的缺失、显式支持与限制解析

use gateway_protocol::anthropic::MessagesModelCapabilities;
use serde_json::json;

#[test]
fn directory_distinguishes_unknown_from_unsupported_capabilities() {
    assert_eq!(
        MessagesModelCapabilities::from_model(&json!({"id":"alias"})),
        Default::default()
    );
    let capabilities = MessagesModelCapabilities::from_model(&json!({
        "id":"any-alias", "max_tokens":64000,
        "capabilities":{
            "thinking":{"supported":true,"types":{"adaptive":{"supported":true},"enabled":{"supported":false},"disabled":{"supported":false}}},
            "effort":{"supported":true,"low":{"supported":true},"high":{"supported":true},"max":{"supported":false},"future":{"supported":true}}
        }
    }));
    assert_eq!(capabilities.adaptive_thinking, Some(true));
    assert_eq!(capabilities.manual_thinking, Some(false));
    assert_eq!(capabilities.disabled_thinking, Some(false));
    assert_eq!(
        capabilities.efforts,
        Some(vec!["low".into(), "high".into()])
    );
    assert_eq!(capabilities.max_tokens, Some(64000));
    assert_eq!(
        MessagesModelCapabilities::from_model(&json!({"max_tokens":0})).max_tokens,
        None
    );
}
