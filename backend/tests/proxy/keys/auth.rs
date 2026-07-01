use codex_proxy_rs::infra::identity::generate_client_api_key;

#[test]
fn client_api_key_should_use_proxy_prefix() {
    let generated = generate_client_api_key();

    assert!(generated.key.starts_with("sk_"));
    assert_eq!(generated.prefix.len(), 12);
}
