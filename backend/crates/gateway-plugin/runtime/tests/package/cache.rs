//! 验证插件包准备使用独立目录，并在释放后回收缓存

use std::sync::Arc;

use gateway_plugin_runtime::{PackageLimits, ValidatedPackage};

#[test]
fn new_preparation_never_modifies_an_existing_version_and_cache_is_reclaimable() {
    let cache = tempfile::tempdir().unwrap();
    let package = Arc::new(
        ValidatedPackage::read(
            crate::support::package(b"hello"),
            None,
            PackageLimits::default(),
        )
        .unwrap(),
    );
    let first = package
        .prepare(cache.path(), &"1.0.0".parse().unwrap())
        .unwrap();
    let second = package
        .prepare(cache.path(), &"1.0.0".parse().unwrap())
        .unwrap();
    assert_ne!(first.directory(), second.directory());
    let first_path = first.directory().to_owned();
    drop(first);
    assert!(!first_path.exists());
    assert_eq!(std::fs::read(second.executable()).unwrap(), b"hello");
}
