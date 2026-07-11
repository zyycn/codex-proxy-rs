use codex_proxy_rs::infra::identity::AccountPseudonymizer;

#[test]
fn account_pseudonyms_should_be_stable_and_domain_separated() {
    let pseudonymizer = AccountPseudonymizer::new([7; 32]);

    let first = pseudonymizer.scoped("session-id", Some("acct-1"), "client-value");
    let repeated = pseudonymizer.scoped("session-id", Some("acct-1"), "client-value");
    let other_account = pseudonymizer.scoped("session-id", Some("acct-2"), "client-value");
    let other_domain = pseudonymizer.scoped("thread-id", Some("acct-1"), "client-value");

    assert_eq!(first, repeated);
    assert_ne!(first, other_account);
    assert_ne!(first, other_domain);
    assert!(!first.contains("client-value"));
}

#[test]
fn installation_ids_should_be_stable_uuid_values_scoped_per_account() {
    let pseudonymizer = AccountPseudonymizer::new([9; 32]);

    let first = pseudonymizer.installation_id("acct-1");
    let repeated = pseudonymizer.installation_id("acct-1");
    let other_account = pseudonymizer.installation_id("acct-2");

    assert_eq!(first, repeated);
    assert_ne!(first, other_account);
    assert!(uuid::Uuid::parse_str(&first).is_ok());
}
