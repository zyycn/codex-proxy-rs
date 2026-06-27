use codex_proxy_rs::infra::identity::ApiKeyHasher;

#[test]
fn client_api_key_has_proxy_prefix_and_verifies_against_hash() {
    let hasher = ApiKeyHasher::new([9u8; 32]);
    let generated = hasher.generate_client_api_key("cursor");
    assert!(generated.key.starts_with("sk_"));
    assert_eq!(generated.prefix.len(), 12);
    assert!(hasher
        .verify_client_api_key(&generated.key, &generated.key_hash)
        .unwrap());
    assert!(!hasher
        .verify_client_api_key("wrong", &generated.key_hash)
        .unwrap());
}
