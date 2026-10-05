//! 验证账号探测可通过对象安全的执行链端口调用

use gateway_core::engine::probe::AccountProbe;

#[test]
fn account_probe_is_an_object_safe_execution_chain_port() {
    fn accepts(_: Option<&dyn AccountProbe>) {}
    accepts(None);
}
