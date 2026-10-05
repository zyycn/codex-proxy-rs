//! 验证插件认证合同限制身份选择并脱敏凭据和主体

use gateway_plugin_sdk::call::frontend_authentication::{
    FrontendAuthenticationRequest, FrontendAuthenticationResult,
};
use serde_json::json;

#[test]
fn authentication_contract_never_accepts_a_plugin_selected_client_key() {
    let result = serde_json::from_value::<FrontendAuthenticationResult>(json!({
        "outcome":"authenticated",
        "principal":"controlled-principal",
        "client_key_id":"attacker-selected-key"
    }));
    assert!(result.is_err());
}

#[test]
fn authentication_debug_output_redacts_credentials_and_principals() {
    let request = FrontendAuthenticationRequest {
        authorization: "Bearer controlled-secret".into(),
    };
    let result = FrontendAuthenticationResult::Authenticated {
        principal: "private-principal".into(),
    };
    let request_debug = format!("{request:?}");
    let result_debug = format!("{result:?}");
    assert!(!request_debug.contains("controlled-secret"));
    assert!(!result_debug.contains("private-principal"));
}
