use codex_proxy_rs::platform::{
    crypto::SecretBox,
    http::auth::{admin_session_id, client_api_key},
    identity::{admin_session::hash_admin_password, api_key::ApiKeyHasher},
    storage::db::connect_sqlite,
};

#[test]
fn platform_exports_foundation_modules() {
    let _secret_box_type = std::any::type_name::<SecretBox>();
    let _hasher = ApiKeyHasher::new([7; 32]);
    let _hash_fn = hash_admin_password;
    let _client_key_fn = client_api_key;
    let _admin_session_fn = admin_session_id;
    let _connect_fn = connect_sqlite;
}
