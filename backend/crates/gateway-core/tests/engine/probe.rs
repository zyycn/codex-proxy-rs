use gateway_core::engine::probe::AccountProbe;

#[test]
fn account_probe_is_an_object_safe_execution_chain_port() {
    fn accepts(_: Option<&dyn AccountProbe>) {}
    accepts(None);
}
