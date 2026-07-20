use gateway_core::engine::execution::provider_failure_affects_circuit;
use gateway_core::error::ProviderErrorKind;

#[test]
fn only_instance_attributable_failures_should_affect_circuit() {
    assert!(provider_failure_affects_circuit(ProviderErrorKind::Timeout));
    assert!(provider_failure_affects_circuit(
        ProviderErrorKind::Transport
    ));
    assert!(!provider_failure_affects_circuit(
        ProviderErrorKind::RateLimited
    ));
    assert!(!provider_failure_affects_circuit(
        ProviderErrorKind::InvalidRequest
    ));
}
