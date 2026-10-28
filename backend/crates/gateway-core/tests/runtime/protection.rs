//! 验证保护策略发布门控、辅助存储失败与周期对账恢复

use super::*;
use crate::engine::account_protection::Feedback;

fn protection_facts(config: u64, protection_revision: u64) -> SnapshotFacts {
    SnapshotFacts::new(
        revision(config),
        revision(config),
        SettingsValues::new(1, "smart", BTreeMap::new(), None, None)
            .with_account_error_protection_revision(protection_revision),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn protection_publication_reuses_generation_and_requires_changed_gate() {
    block_on(async {
        let store = Arc::new(TestSnapshotStore::new(Ok(protection_facts(1, 1))));
        let handle = RuntimeSnapshotHandle::default();
        let protection = Arc::new(Feedback::default());
        let publisher = RuntimeSnapshotPublisher::new(
            Arc::new(compiler(store.clone())),
            handle.clone(),
            Arc::new(TestSnapshotSubscriptions::default()),
        )
        .with_account_error_protection(protection.clone());
        publisher.refresh().await.unwrap();
        assert_eq!(*protection.publications.lock().unwrap(), vec![1]);
        publisher.refresh().await.unwrap();
        assert_eq!(*protection.publications.lock().unwrap(), vec![1]);
        protection.publish_fails.store(true, Ordering::SeqCst);
        let (task, context) = reconciliation_task(&publisher);
        task.run_cycle(context).await.unwrap();
        assert!(handle.acquire().is_ok());
        assert_eq!(*protection.publications.lock().unwrap(), vec![1, 1]);
        *store.facts.lock().unwrap() = Ok(protection_facts(2, 2));
        *store.current_revision.lock().unwrap() = Ok(revision(2));
        publisher.publish_committed(revision(2)).await;
        assert!(handle.acquire().is_err());
        assert!(handle.snapshot_for_diagnostics().is_none());
        protection.publish_fails.store(false, Ordering::SeqCst);
        let (task, context) = reconciliation_task(&publisher);
        task.run_cycle(context).await.unwrap();
        assert_eq!(handle.acquire().unwrap().revision(), revision(2));
        assert_eq!(*protection.publications.lock().unwrap(), vec![1, 1, 2, 2]);
    });
}
