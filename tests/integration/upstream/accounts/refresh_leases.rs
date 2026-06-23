use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::{
    infra::{crypto::SecretBox, database::connect_sqlite},
    upstream::accounts::{
        model::AccountStatus,
        store::{NewAccount, SqliteAccountStore},
        token_refresh::RefreshLeaseStore,
    },
};
use secrecy::SecretString;

#[tokio::test]
async fn refresh_lease_store_should_acquire_release_and_respect_expiry() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("refresh-leases.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let secret_box = SecretBox::new([31u8; 32]);
    let accounts = SqliteAccountStore::new(pool.clone(), secret_box);
    accounts
        .insert(NewAccount {
            id: "acct-lease".to_string(),
            email: None,
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access".to_string().into()),
            refresh_token: Some(SecretString::new("refresh".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        })
        .await
        .expect("account should be inserted");
    let leases = RefreshLeaseStore::new(pool);
    let now = Utc.with_ymd_and_hms(2026, 6, 19, 8, 0, 0).unwrap();
    let first_expires_at = now + Duration::minutes(5);
    let second_expires_at = now + Duration::minutes(10);

    assert!(leases
        .try_acquire("acct-lease", "owner-a", first_expires_at, now)
        .await
        .expect("first owner should acquire"));
    assert!(!leases
        .try_acquire("acct-lease", "owner-b", second_expires_at, now)
        .await
        .expect("second owner should be blocked"));
    assert!(leases
        .try_acquire(
            "acct-lease",
            "owner-b",
            second_expires_at,
            first_expires_at + Duration::seconds(1),
        )
        .await
        .expect("second owner should acquire expired lease"));
    assert!(!leases
        .release("acct-lease", "owner-a")
        .await
        .expect("wrong owner should not release"));
    assert!(leases
        .release("acct-lease", "owner-b")
        .await
        .expect("current owner should release"));
    assert!(leases
        .try_acquire("acct-lease", "owner-c", second_expires_at, now)
        .await
        .expect("lease should be acquirable after release"));
}
