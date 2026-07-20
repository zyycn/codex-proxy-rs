use std::str::FromStr as _;

use chrono::{TimeDelta, Utc};
use gateway_core::engine::credential::OpaqueProviderData;

use gateway_admin::{
    model::{
        PageSize, Revision,
        auth::LoginCommand,
        client_keys::ClientKeyPageSize,
        observability::{DecimalAmount, RequestOutcome, TimeRange},
        provider_credentials::{CredentialCommitGuard, ProviderDocument},
        settings::AdminApiKey,
    },
    ports::store::{
        AccountStore, AuthStore, CatalogStore, ClientKeyStore, ObservabilityStore, SettingsStore,
    },
};

mod use_case;

#[test]
fn revision_should_reject_zero() {
    assert!(Revision::new(0).is_err());
}

#[test]
fn page_size_should_accept_frozen_upper_bound() {
    assert_eq!(PageSize::new(200).map(PageSize::get), Ok(200));
}

#[test]
fn page_size_should_reject_value_above_frozen_upper_bound() {
    assert!(PageSize::new(201).is_err());
}

#[test]
fn client_key_page_size_should_keep_the_full_nonzero_u16_contract() {
    assert!(ClientKeyPageSize::new(0).is_err());
    assert_eq!(
        ClientKeyPageSize::new(u16::MAX).map(ClientKeyPageSize::get),
        Ok(u16::MAX)
    );
}

#[test]
fn request_outcome_filter_should_preserve_known_and_bounded_other_values() {
    let known = RequestOutcome::new("succeeded").expect("known outcome");
    let other = RequestOutcome::new("provider_future_state").expect("other outcome");

    assert_eq!(known, RequestOutcome::Succeeded);
    assert!(matches!(&other, RequestOutcome::Other(_)));
    assert_eq!(other.as_str(), "provider_future_state");
    assert!(RequestOutcome::new("").is_err());
    assert!(RequestOutcome::new("a".repeat(RequestOutcome::MAX_BYTES + 1)).is_err());
    assert!(RequestOutcome::new("future\nstate").is_err());
}

#[test]
fn time_range_should_accept_exactly_366_days() {
    let end = Utc::now();
    assert!(TimeRange::new(end - TimeDelta::days(366), end).is_ok());
}

#[test]
fn time_range_should_reject_more_than_366_days() {
    let end = Utc::now();
    assert!(TimeRange::new(end - TimeDelta::days(366) - TimeDelta::seconds(1), end).is_err());
}

#[test]
fn decimal_amount_should_canonicalize_redundant_zeroes() {
    assert_eq!(
        DecimalAmount::from_str("00012.34000").map(|amount| amount.to_string()),
        Ok("12.34".to_owned())
    );
}

#[test]
fn admin_api_key_debug_should_redact_plaintext() {
    let key = AdminApiKey::new("secret-admin-key");
    assert!(!format!("{key:?}").contains("secret-admin-key"));
}

#[test]
fn login_command_debug_should_redact_password() {
    let command = LoginCommand {
        username: None,
        password: "secret-password".to_owned(),
        source: "127.0.0.1".to_owned(),
    };
    assert!(!format!("{command:?}").contains("secret-password"));
}

#[test]
fn all_store_capability_traits_should_be_object_safe() {
    fn assert_object_safe<T: ?Sized + Send + Sync>() {}

    assert_object_safe::<dyn AccountStore>();
    assert_object_safe::<dyn AuthStore>();
    assert_object_safe::<dyn CatalogStore>();
    assert_object_safe::<dyn ClientKeyStore>();
    assert_object_safe::<dyn ObservabilityStore>();
    assert_object_safe::<dyn SettingsStore>();

    fn assert_send_object_safe<T: ?Sized + Send>() {}
    assert_send_object_safe::<dyn CredentialCommitGuard>();
}

#[test]
fn provider_document_debug_should_not_expose_opaque_material() {
    let document = ProviderDocument::new(OpaqueProviderData::new(Default::default()));
    assert_eq!(
        format!("{document:?}"),
        "ProviderDocument([PROVIDER_OWNED])"
    );
}
