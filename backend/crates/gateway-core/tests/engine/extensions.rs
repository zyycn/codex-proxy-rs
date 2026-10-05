//! 执行测试共用的扩展代次引用与就绪租约替身

use std::sync::Arc;

use gateway_core::runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference};

struct ReadyLease;

impl ExtensionSetLease for ReadyLease {
    fn is_ready(&self) -> bool {
        true
    }
}

pub(super) fn reference(id: &str) -> ExtensionSetReference {
    let id = ExtensionSetId::new(id.into()).unwrap();
    ExtensionSetReference::new(id, Arc::new(ReadyLease))
}
