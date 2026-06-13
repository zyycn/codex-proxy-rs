use codex_proxy_rs::codex::gateway::{
    fingerprint::model::Fingerprint, oauth::TokenPair, protocol::schema::ResponseFormat,
    transport::client::CodexBackendClient,
};

#[test]
fn gateway_exports_chatgpt_integration_modules() {
    let _fingerprint_type = std::any::type_name::<Fingerprint>();
    let _token_pair_type = std::any::type_name::<TokenPair>();
    let _response_format_type = std::any::type_name::<ResponseFormat>();
    let _client_type = std::any::type_name::<CodexBackendClient>();
}
