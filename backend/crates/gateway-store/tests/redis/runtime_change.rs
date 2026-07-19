use gateway_store::{Revision, redis::RuntimeChange};

#[test]
fn runtime_change_does_not_expose_raw_account_id() {
    let account_id = "real-account-id";
    let change = RuntimeChange::provider_account_changed(
        account_id,
        Revision::new(1).expect("positive revision"),
    )
    .expect("valid change");
    assert!(!format!("{change:?}").contains(account_id));
}
