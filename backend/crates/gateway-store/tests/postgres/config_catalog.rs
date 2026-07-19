use gateway_store::postgres::NewProviderInstance;

#[test]
fn provider_instance_rejects_unstable_kind() {
    let instance = NewProviderInstance {
        id: "instance-1".to_owned(),
        provider_kind: "xAI!".to_owned(),
        name: "xAI".to_owned(),
        base_url: "https://example.invalid".to_owned(),
    };
    assert!(instance.validate().is_err());
}
