use gateway_host::HostBundle;

#[test]
fn host_bundle_serve_is_a_consuming_process_entrypoint() {
    let _serve = HostBundle::serve;

    assert_eq!(std::mem::size_of_val(&_serve), 0);
}
