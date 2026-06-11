use codex_proxy_rs::auth::admin_session::{hash_admin_password, verify_admin_password};

#[test]
fn admin_password_hash_is_not_a_client_api_key() {
    let hash = hash_admin_password("correct horse battery staple").unwrap();
    assert!(verify_admin_password("correct horse battery staple", &hash).unwrap());
    assert!(!verify_admin_password("cpr_fake_client_key", &hash).unwrap());
}
