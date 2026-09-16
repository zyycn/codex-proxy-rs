use std::{sync::Arc, time::Duration};

use gateway_admin::{
    AdminBundle, AdminServices,
    model::{
        AdminErrorKind, MutationActor, MutationContext,
        import_tasks::{ImportTaskInput, SubmitImportTask},
        provider_credentials::ImportCredentials,
    },
    ports::provider::ProviderAdminErrorKind,
};
use gateway_core::{
    lifecycle::CancellationToken,
    routing::ProviderKind,
    task::{WorkerContribution, WorkerKind, WorkerRunnable},
};
use uuid::Uuid;

use super::{
    AdminHarness,
    accounts::{
        EventLog, FakeAccountStore, FakeProviderAdmin, context, document, events, recorded,
    },
};

fn command(id: Uuid, count: usize) -> SubmitImportTask {
    let context = context("task-submit");
    SubmitImportTask {
        submission_id: id,
        fingerprint: [1; 32],
        context: context.clone(),
        items: (0..count)
            .map(|_| ImportTaskInput {
                provider: ProviderKind::new("openai").unwrap(),
                command: ImportCredentials {
                    outbound_proxy_id: None,
                    settings: None,
                    context: context.clone(),
                    document: document(),
                },
            })
            .collect(),
    }
}

async fn harness() -> (AdminBundle, Arc<FakeProviderAdmin>, EventLog) {
    let log = events();
    let provider = FakeProviderAdmin::new("openai", log.clone());
    let bundle = AdminHarness::new()
        .provider(provider.clone())
        .accounts(FakeAccountStore::new("openai", log.clone()))
        .build_bundle()
        .await;
    (bundle, provider, log)
}

fn start(bundle: &mut AdminBundle) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let registration = bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.kind() == WorkerKind::AccountImport =>
            {
                Some(registration)
            }
            _ => None,
        })
        .expect("import registration");
    let WorkerRunnable::Daemon { task, .. } = registration.runnable else {
        panic!("import daemon")
    };
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let handle = tokio::spawn(async move {
        task.run(token).await.expect("import daemon exits cleanly");
    });
    (cancellation, handle)
}

async fn wait_started(log: &EventLog, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorded(log)
            .iter()
            .filter(|event| **event == "provider.prepare_import")
            .count()
            < count
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("imports started");
}

async fn wait_finished(services: &AdminServices, id: Uuid) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while services
            .import_tasks()
            .detail(&context("poll"), id)
            .unwrap()
            .summary
            .finished_at
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("imports finished");
}

#[tokio::test]
async fn retry_should_return_the_same_task_but_reject_changed_content() {
    let (bundle, _, _) = harness().await;
    let services = bundle.services();
    let id = Uuid::now_v7();
    let first = services.import_tasks().submit(command(id, 2)).unwrap();
    let retry = services.import_tasks().submit(command(id, 2)).unwrap();
    assert_eq!(first.task_id, retry.task_id);
    assert_eq!(
        services
            .import_tasks()
            .list(&context("reopened-page"))
            .len(),
        1
    );
    let mut changed = command(id, 2);
    changed.fingerprint = [2; 32];
    assert_eq!(
        services.import_tasks().submit(changed).unwrap_err().kind(),
        AdminErrorKind::Conflict
    );
}

#[tokio::test]
async fn task_reads_and_stop_should_be_isolated_by_owner() {
    let (bundle, _, _) = harness().await;
    let services = bundle.services();
    let task = services
        .import_tasks()
        .submit(command(Uuid::now_v7(), 1))
        .unwrap();
    let other = MutationContext {
        actor: MutationActor::AdminSession {
            admin_user_id: "another-admin".to_owned(),
        },
        request_id: "other-session".to_owned(),
    };
    assert!(services.import_tasks().list(&other).is_empty());
    assert_eq!(
        services
            .import_tasks()
            .detail(&other, task.task_id)
            .unwrap_err()
            .kind(),
        AdminErrorKind::NotFound
    );
    assert_eq!(
        services
            .import_tasks()
            .stop(&other, task.task_id)
            .unwrap_err()
            .kind(),
        AdminErrorKind::NotFound
    );
}

#[tokio::test]
async fn stopping_should_skip_pending_items_and_allow_three_in_flight_to_commit() {
    let (mut bundle, provider, log) = harness().await;
    let gate = provider.block_imports();
    let services = bundle.services();
    let task = services
        .import_tasks()
        .submit(command(Uuid::now_v7(), 8))
        .unwrap();
    let (cancel, worker) = start(&mut bundle);
    wait_started(&log, 3).await;
    let stopped = services
        .import_tasks()
        .stop(&context("stop"), task.task_id)
        .unwrap();
    assert_eq!(stopped.summary.counts.running, 3);
    assert_eq!(stopped.summary.counts.skipped, 5);
    gate.add_permits(3);
    wait_finished(&services, task.task_id).await;
    let finished = services
        .import_tasks()
        .detail(&context("new-page"), task.task_id)
        .unwrap();
    assert_eq!(finished.summary.counts.imported_accounts, 3);
    assert_eq!(
        recorded(&log)
            .iter()
            .filter(|event| **event == "provider.prepare_import")
            .count(),
        3
    );
    cancel.cancel();
    worker.await.unwrap();
}

#[tokio::test]
async fn concurrency_limit_should_be_shared_across_tasks() {
    let (mut bundle, provider, log) = harness().await;
    let gate = provider.block_imports();
    let services = bundle.services();
    let first = services
        .import_tasks()
        .submit(command(Uuid::now_v7(), 6))
        .unwrap();
    let second = services
        .import_tasks()
        .submit(command(Uuid::now_v7(), 6))
        .unwrap();
    let (cancel, worker) = start(&mut bundle);
    wait_started(&log, 3).await;
    let summaries = services.import_tasks().list(&context("poll"));
    assert_eq!(
        summaries
            .iter()
            .map(|task| task.counts.running)
            .sum::<usize>(),
        3
    );
    assert!(summaries.iter().all(|task| task.counts.running > 0));
    gate.add_permits(12);
    wait_finished(&services, first.task_id).await;
    wait_finished(&services, second.task_id).await;
    cancel.cancel();
    worker.await.unwrap();
}

#[tokio::test]
async fn failed_and_unknown_items_should_not_abort_the_batch_or_retry_credentials() {
    for (kind, failed, unknown) in [
        (ProviderAdminErrorKind::Invalid, 1, 0),
        (ProviderAdminErrorKind::Ambiguous, 0, 1),
    ] {
        let (mut bundle, provider, log) = harness().await;
        provider.fail_next(kind);
        let services = bundle.services();
        let task = services
            .import_tasks()
            .submit(command(Uuid::now_v7(), 4))
            .unwrap();
        let (cancel, worker) = start(&mut bundle);
        wait_finished(&services, task.task_id).await;
        let result = services
            .import_tasks()
            .detail(&context("poll"), task.task_id)
            .unwrap();
        assert_eq!(
            (
                result.summary.counts.succeeded,
                result.summary.counts.failed,
                result.summary.counts.unknown
            ),
            (3, failed, unknown)
        );
        assert_eq!(
            recorded(&log)
                .iter()
                .filter(|event| **event == "provider.prepare_import")
                .count(),
            4
        );
        cancel.cancel();
        worker.await.unwrap();
    }
}

#[tokio::test]
async fn shutdown_should_drain_running_items_and_reject_new_work() {
    let (mut bundle, provider, log) = harness().await;
    let gate = provider.block_imports();
    let services = bundle.services();
    let task = services
        .import_tasks()
        .submit(command(Uuid::now_v7(), 7))
        .unwrap();
    let (cancel, worker) = start(&mut bundle);
    wait_started(&log, 3).await;
    cancel.cancel();
    tokio::task::yield_now().await;
    assert_eq!(
        services
            .import_tasks()
            .submit(command(Uuid::now_v7(), 1))
            .unwrap_err()
            .kind(),
        AdminErrorKind::Unavailable
    );
    assert!(!worker.is_finished());
    gate.add_permits(3);
    worker.await.unwrap();
    let result = services
        .import_tasks()
        .detail(&context("poll"), task.task_id)
        .unwrap();
    assert_eq!(
        (
            result.summary.counts.succeeded,
            result.summary.counts.skipped
        ),
        (3, 4)
    );
}

#[tokio::test(start_paused = true)]
async fn completed_records_should_expire_and_new_process_should_start_empty() {
    let (bundle, _, _) = harness().await;
    let services = bundle.services();
    let task = services
        .import_tasks()
        .submit(command(Uuid::now_v7(), 2))
        .unwrap();
    services
        .import_tasks()
        .stop(&context("stop"), task.task_id)
        .unwrap();
    tokio::time::advance(Duration::from_secs(3601)).await;
    assert!(services.import_tasks().list(&context("poll")).is_empty());
    assert_eq!(
        services
            .import_tasks()
            .detail(&context("poll"), task.task_id)
            .unwrap_err()
            .kind(),
        AdminErrorKind::NotFound
    );
    let (fresh, _, _) = harness().await;
    assert!(
        fresh
            .services()
            .import_tasks()
            .list(&context("poll"))
            .is_empty()
    );
}

#[tokio::test]
async fn queue_should_reject_excess_work_before_starting_any_imports() {
    let (bundle, _, log) = harness().await;
    let services = bundle.services();
    assert_eq!(
        services
            .import_tasks()
            .submit(command(Uuid::now_v7(), 201))
            .unwrap_err()
            .kind(),
        AdminErrorKind::Invalid
    );
    for _ in 0..8 {
        services
            .import_tasks()
            .submit(command(Uuid::now_v7(), 1))
            .unwrap();
    }
    assert_eq!(
        services
            .import_tasks()
            .submit(command(Uuid::now_v7(), 1))
            .unwrap_err()
            .kind(),
        AdminErrorKind::RateLimited
    );
    assert!(recorded(&log).is_empty());
}
