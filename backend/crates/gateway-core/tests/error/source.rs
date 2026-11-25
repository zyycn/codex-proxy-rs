//! 对应生产职责的行为测试

use super::*;

#[test]
fn source_chain_survives_domain_conversion_and_snapshot_without_entering_formatting() {
    use gateway_core::error::{StoreError, StoreErrorKind};
    use gateway_core::provider_ports::{ProviderStoreError, ProviderStoreErrorKind};
    use std::error::Error as _;

    let store = StoreError::caused_by(
        StoreErrorKind::Unavailable,
        std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "PRIVATE_DB_CAUSE"),
    );
    let port =
        ProviderStoreError::caused_by(ProviderStoreErrorKind::Unavailable, "select account", store);
    let error = ProviderError::new(
        ProviderErrorKind::ProviderInfrastructureUnavailable,
        UpstreamSendState::NotSent,
    )
    .with_source(port);
    let snapshot = error.stable_snapshot();
    drop(error);

    let port = snapshot
        .source()
        .unwrap()
        .downcast_ref::<ProviderStoreError>()
        .unwrap();
    let store = port.source().unwrap().downcast_ref::<StoreError>().unwrap();
    let original = store
        .source()
        .unwrap()
        .downcast_ref::<std::io::Error>()
        .unwrap();
    assert_eq!(original.kind(), std::io::ErrorKind::ConnectionRefused);
    assert_eq!(original.to_string(), "PRIVATE_DB_CAUSE");
    assert!(
        !format!("{snapshot:?} {snapshot} {port:?} {port} {store:?} {store}")
            .contains("PRIVATE_DB_CAUSE")
    );
    assert_eq!(snapshot.send_state(), UpstreamSendState::NotSent);
}
