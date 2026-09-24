use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use gateway_core::runtime::{
    RuntimeSnapshotHandle,
    extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};

struct Lease {
    ready: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
}
impl ExtensionSetLease for Lease {
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn publication_holds_the_generation_until_its_last_inflight_snapshot_is_released() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let lease = Arc::new(Lease {
        ready: Arc::new(AtomicBool::new(true)),
        dropped: Arc::clone(&dropped),
    });
    let reference =
        ExtensionSetReference::new(ExtensionSetId::new("generation-one".into()).unwrap(), lease);
    let handle =
        RuntimeSnapshotHandle::new(super::empty_snapshot(1).with_extensions(Some(reference)));
    let inflight = handle.acquire().unwrap();
    handle.publish(super::empty_snapshot(2));
    assert_eq!(
        inflight.extensions().unwrap().id().as_str(),
        "generation-one"
    );
    assert!(handle.acquire().unwrap().extensions().is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(inflight);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_generation_blocks_new_requests_without_destroying_an_inflight_reference() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicBool::new(true));
    let lease = Arc::new(Lease {
        ready: Arc::clone(&ready),
        dropped: Arc::clone(&dropped),
    });
    let handle = RuntimeSnapshotHandle::new(super::empty_snapshot(1).with_extensions(Some(
        ExtensionSetReference::new(ExtensionSetId::new("generation-one".into()).unwrap(), lease),
    )));
    let inflight = handle.acquire().unwrap();
    ready.store(false, Ordering::SeqCst);
    assert!(handle.acquire().is_err());
    let diagnostic = handle
        .snapshot_for_diagnostics()
        .expect("unready publication remains observable to the control plane");
    assert_eq!(diagnostic.revision().get(), 1);
    assert_eq!(
        diagnostic.extensions().unwrap().id().as_str(),
        "generation-one"
    );
    drop(diagnostic);
    assert_eq!(
        inflight.extensions().unwrap().id().as_str(),
        "generation-one"
    );
    handle.suspend();
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(inflight);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn an_isolated_extension_fault_keeps_new_requests_and_host_health_available() {
    use gateway_core::health::{HealthProbe, HealthState};
    struct IsolatedFault;
    impl ExtensionSetLease for IsolatedFault {
        fn is_ready(&self) -> bool {
            false
        }
        fn can_serve(&self) -> bool {
            true
        }
    }
    let reference = ExtensionSetReference::new(
        ExtensionSetId::new("isolated-fault".into()).unwrap(),
        Arc::new(IsolatedFault),
    );
    assert!(!reference.is_ready());
    let handle =
        RuntimeSnapshotHandle::new(super::empty_snapshot(1).with_extensions(Some(reference)));
    assert!(handle.acquire().is_ok());
    assert!(matches!(
        futures::executor::block_on(handle.check()),
        HealthState::Healthy
    ));
    handle.suspend();
    assert!(
        handle.acquire().is_err(),
        "unconfirmed host configuration still fails closed"
    );
}
