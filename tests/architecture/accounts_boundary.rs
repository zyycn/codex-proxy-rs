use codex_proxy_core::{
    accounts::jwt::{jwt_expiry, JwtExpiry},
    auth::oauth::{RefreshPolicy, RefreshTrigger},
    gateway::conversation::{build_conversation_identity, ConversationIdentity},
};

#[test]
fn core_exports_account_domain_helpers() {
    let _expiry_type = std::any::type_name::<JwtExpiry>();
    let _identity_type = std::any::type_name::<ConversationIdentity>();
    let _refresh_policy_type = std::any::type_name::<RefreshPolicy>();
    let _refresh_trigger_type = std::any::type_name::<RefreshTrigger>();
    let _jwt_expiry_fn = jwt_expiry;
    let _identity_fn = build_conversation_identity;
}
