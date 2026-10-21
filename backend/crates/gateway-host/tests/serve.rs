//! 验证统一关闭入口跳过会话等待、保留后台落盘及清理超时

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::{Router, routing::get};
use futures::future::BoxFuture;
use gateway_admin::ports::system::{
    SystemOperationError, SystemRestartPreflight, SystemUpdateCandidate,
};
use gateway_core::diagnostics::{OperationalDiagnostics, OperationalFailure};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::task::{
    DaemonRestartPolicy, DaemonTask, WorkerContribution, WorkerDaemonContext, WorkerId, WorkerKind,
    WorkerLeaderLeasePort, WorkerLeaseAcquisition, WorkerLeaseError, WorkerLeaseRequest,
    WorkerRegistration, WorkerRunnable, WorkerTaskError,
};
use gateway_host::HostBundle;
use gateway_host::serve::bind_listener;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Semaphore, mpsc};

use crate::support::host::{configuration, run_in_child};

#[test]
fn host_bundle_serve_is_a_consuming_process_entrypoint() {
    let _serve = HostBundle::serve;

    assert_eq!(std::mem::size_of_val(&_serve), 0);
}

#[tokio::test]
async fn bind_listener_should_retry_until_previous_listener_releases_port() {
    let holder = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("holder listener");
    let address = holder.local_addr().expect("holder address").to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(holder);
    });

    let listener = tokio::time::timeout(Duration::from_secs(8), bind_listener(&address))
        .await
        .expect("bound within the retry window")
        .expect("bound after the previous listener released the port");

    assert_eq!(
        listener.local_addr().expect("bound address").to_string(),
        address
    );
}

#[test]
fn cancellation_skips_connections_but_flushes_and_joins_workers() {
    run_in_child(
        "serve::cancellation_skips_connections_but_flushes_and_joins_workers",
        shutdown_with_pending_request(ShutdownTrigger::Cancellation),
    );
}

#[test]
fn self_restart_skips_connections_but_flushes_and_joins_workers() {
    run_in_child(
        "serve::self_restart_skips_connections_but_flushes_and_joins_workers",
        shutdown_with_pending_request(ShutdownTrigger::SelfRestart),
    );
}

#[cfg(unix)]
#[test]
fn sigterm_skips_connections_but_flushes_and_joins_workers() {
    run_in_child(
        "serve::sigterm_skips_connections_but_flushes_and_joins_workers",
        shutdown_with_pending_request(ShutdownTrigger::Signal("-TERM")),
    );
}

#[cfg(unix)]
#[test]
fn sigint_skips_connections_but_flushes_and_joins_workers() {
    run_in_child(
        "serve::sigint_skips_connections_but_flushes_and_joins_workers",
        shutdown_with_pending_request(ShutdownTrigger::Signal("-INT")),
    );
}

enum ShutdownTrigger {
    Cancellation,
    SelfRestart,
    #[cfg(unix)]
    Signal(&'static str),
}

async fn shutdown_with_pending_request(trigger: ShutdownTrigger) {
    let directory = tempfile::tempdir().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut config = configuration(directory.path());
    config.listen.port = address.port();
    config.system_update.deployment_mode = "docker".to_owned();
    config.system_update.build_type = "source".to_owned();
    config.system_update.self_restart_enabled = true;
    config.system_update.update_lock_file = directory.path().join("update.lock");
    let host = gateway_host::initialize(config).await.unwrap();
    drop(reservation);
    let system = host.system_operations();
    let cancellation = host.cancellation();
    let connections = host.connection_lifecycle();
    let guard = connections.try_register().unwrap();
    let probe = start_workers(&host, true);
    probe.started.cancelled().await;
    let (router, entered, _finish_request) = request_router(&host, &probe);
    let server = tokio::spawn(host.serve(router));
    let client = connect_request(address).await;
    entered.cancelled().await;
    probe.sender.send(7).unwrap();

    match trigger {
        ShutdownTrigger::Cancellation => cancellation.cancel(),
        ShutdownTrigger::SelfRestart => {
            system.restart(Arc::new(AllowRestart)).await.unwrap();
        }
        #[cfg(unix)]
        ShutdownTrigger::Signal(signal) => {
            // 真实请求进入后信号监听已被 poll，只向隔离的当前测试子进程发送信号
            assert!(
                std::process::Command::new("kill")
                    .args([signal, &std::process::id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
    tokio::time::timeout(Duration::from_secs(2), probe.cancelled.cancelled())
        .await
        .expect("shutdown must not wait for the blocked HTTP request or live guard");
    assert!(connections.is_draining());
    assert!(connections.try_register().is_err());
    drop(guard);
    assert!(connections.try_register().is_err());
    assert!(
        tokio::net::TcpStream::connect(address).await.is_err(),
        "listener must close before worker cleanup finishes"
    );
    assert!(
        !server.is_finished(),
        "worker cleanup still needs to finish"
    );
    probe.finish_shutdown.add_permits(1);
    server.await.unwrap().unwrap();
    assert_eq!(*probe.writes.lock().unwrap(), [7]);
    assert!(probe.finished.is_cancelled());
    // 请求仍未结束，独立进程退出时终止连接和挂起 handler
    drop(client);
}

#[test]
fn shutdown_aborts_workers_that_exceed_the_cleanup_budget() {
    run_in_child(
        "serve::shutdown_aborts_workers_that_exceed_the_cleanup_budget",
        async {
            let directory = tempfile::tempdir().unwrap();
            let (host, address) = listening_host(directory.path()).await;
            let cancellation = host.cancellation();
            let probe = start_workers(&host, true);
            probe.started.cancelled().await;
            let (router, entered, _finish_request) = request_router(&host, &probe);
            let server = tokio::spawn(host.serve(router));
            let client = connect_request(address).await;
            entered.cancelled().await;
            cancellation.cancel();

            tokio::time::timeout(Duration::from_secs(3), server)
                .await
                .expect("worker cleanup budget must bound process shutdown")
                .unwrap()
                .unwrap();
            probe.dropped.cancelled().await;
            assert!(probe.cancelled.is_cancelled());
            assert!(!probe.finished.is_cancelled());
            drop(client);
        },
    );
}

struct AllowRestart;

#[async_trait::async_trait]
impl SystemRestartPreflight for AllowRestart {
    async fn prepare(&self, _: Option<SystemUpdateCandidate>) -> Result<(), SystemOperationError> {
        Ok(())
    }
}

#[test]
fn bind_failure_cancels_and_joins_workers_before_returning() {
    run_in_child(
        "serve::bind_failure_cancels_and_joins_workers_before_returning",
        async {
            let directory = tempfile::tempdir().unwrap();
            let mut config = configuration(directory.path());
            config.listen.host = "invalid address".to_owned();
            let host = gateway_host::initialize(config).await.unwrap();
            let cancellation = host.cancellation();
            let probe = start_workers(&host, false);
            probe.started.cancelled().await;

            let result = host.serve(Router::new()).await;

            assert!(matches!(
                result,
                Err(gateway_host::HostError::Serve(
                    gateway_host::serve::ServeError::Bind(_)
                ))
            ));
            assert!(cancellation.is_cancelled());
            assert!(probe.finished.is_cancelled());
        },
    );
}

#[test]
fn dropping_serve_cancels_public_operations_and_aborts_workers() {
    run_in_child(
        "serve::dropping_serve_cancels_public_operations_and_aborts_workers",
        async {
            let directory = tempfile::tempdir().unwrap();
            let (host, address) = listening_host(directory.path()).await;
            let cancellation = host.cancellation();
            let probe = start_workers(&host, true);
            probe.started.cancelled().await;
            let server = tokio::spawn(host.serve(Router::new()));
            let client = connect_request(address).await;

            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
            probe.dropped.cancelled().await;

            assert!(cancellation.is_cancelled());
            assert!(!probe.finished.is_cancelled());
            drop(client);
        },
    );
}

#[test]
fn dropping_serve_during_worker_shutdown_aborts_the_joined_worker() {
    run_in_child(
        "serve::dropping_serve_during_worker_shutdown_aborts_the_joined_worker",
        async {
            let directory = tempfile::tempdir().unwrap();
            let (host, address) = listening_host(directory.path()).await;
            let cancellation = host.cancellation();
            let probe = start_workers(&host, true);
            probe.started.cancelled().await;
            let server = tokio::spawn(host.serve(Router::new()));
            let client = connect_request(address).await;

            cancellation.cancel();
            probe.cancelled.cancelled().await;
            assert!(!probe.finished.is_cancelled());
            assert!(
                !server.is_finished(),
                "worker shutdown must still be waiting"
            );
            // Worker 已观察独立的取消信号，serve 正持有 shutdown 的 join future
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
            tokio::time::timeout(Duration::from_secs(1), probe.dropped.cancelled())
                .await
                .expect("aborting shutdown must not detach the worker");

            assert!(!probe.finished.is_cancelled());
            drop(client);
        },
    );
}

async fn listening_host(directory: &std::path::Path) -> (HostBundle, std::net::SocketAddr) {
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let mut config = configuration(directory);
    config.listen.port = address.port();
    let host = gateway_host::initialize(config).await.unwrap();
    drop(reservation);
    (host, address)
}

async fn connect_request(address: std::net::SocketAddr) -> tokio::net::TcpStream {
    let mut client = loop {
        match tokio::net::TcpStream::connect(address).await {
            Ok(client) => break client,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => panic!("connect host: {error}"),
        }
    };
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    client
}

fn request_router(
    host: &HostBundle,
    probe: &WriterProbe,
) -> (Router, CancellationToken, Arc<Semaphore>) {
    let entered = CancellationToken::new();
    let finish = Arc::new(Semaphore::new(0));
    let connections = host.connection_lifecycle();
    let sender = probe.sender.clone();
    let router = Router::new().route(
        "/",
        get({
            let entered = entered.clone();
            let finish = Arc::clone(&finish);
            move || {
                let entered = entered.clone();
                let finish = Arc::clone(&finish);
                let connections = Arc::clone(&connections);
                let sender = sender.clone();
                async move {
                    let _connection = connections.try_register().unwrap();
                    entered.cancel();
                    finish.acquire().await.unwrap().forget();
                    // 对应请求结束后写入终态的时机；关闭开始后的写入允许被拒绝
                    if sender.send(1).is_ok() {
                        "recorded"
                    } else {
                        "writer stopped"
                    }
                }
            }
        }),
    );
    (router, entered, finish)
}

struct WriterProbe {
    sender: mpsc::UnboundedSender<u8>,
    writes: Arc<Mutex<Vec<u8>>>,
    started: CancellationToken,
    cancelled: CancellationToken,
    finish_shutdown: Arc<Semaphore>,
    finished: CancellationToken,
    dropped: CancellationToken,
}

struct WriterTask {
    received: tokio::sync::Mutex<mpsc::UnboundedReceiver<u8>>,
    probe: Arc<WriterProbe>,
}

struct TaskLifetime(CancellationToken);

impl Drop for TaskLifetime {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl DaemonTask for WriterTask {
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let cancellation = context.cancellation();
            let _lifetime = TaskLifetime(self.probe.dropped.clone());
            let mut received = self.received.lock().await;
            self.probe.started.cancel();
            loop {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => break,
                    value = received.recv() => {
                        self.probe.writes.lock().unwrap().push(value.unwrap());
                    }
                }
            }
            self.probe.cancelled.cancel();
            received.close();
            while let Some(value) = received.recv().await {
                self.probe.writes.lock().unwrap().push(value);
            }
            self.probe.finish_shutdown.acquire().await.unwrap().forget();
            self.probe.finished.cancel();
            Ok(())
        })
    }
}

struct IdleTask;

impl DaemonTask for IdleTask {
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let cancellation = context.cancellation();
            cancellation.cancelled().await;
            Ok(())
        })
    }
}

struct UnusedLease;

impl WorkerLeaderLeasePort for UnusedLease {
    fn try_acquire(
        &self,
        _: WorkerLeaseRequest,
    ) -> BoxFuture<'_, Result<WorkerLeaseAcquisition, WorkerLeaseError>> {
        Box::pin(async { Err(WorkerLeaseError::safe("daemon must not acquire a lease")) })
    }
}

struct Diagnostics;

#[async_trait::async_trait]
impl OperationalDiagnostics for Diagnostics {
    async fn record_failure(
        &self,
        _: OperationalFailure,
    ) -> Result<(), gateway_core::error::StoreError> {
        Ok(())
    }
}

fn start_workers(host: &HostBundle, hold_shutdown: bool) -> Arc<WriterProbe> {
    let (sender, received) = mpsc::unbounded_channel();
    let probe = Arc::new(WriterProbe {
        sender,
        writes: Arc::new(Mutex::new(Vec::new())),
        started: CancellationToken::new(),
        cancelled: CancellationToken::new(),
        finish_shutdown: Arc::new(Semaphore::new(usize::from(!hold_shutdown))),
        finished: CancellationToken::new(),
        dropped: CancellationToken::new(),
    });
    let mut writer = Some(WriterTask {
        received: tokio::sync::Mutex::new(received),
        probe: Arc::clone(&probe),
    });
    let plan = WorkerKind::ALL
        .into_iter()
        .map(|kind| {
            let task: Box<dyn DaemonTask> = if kind == WorkerKind::OpsFlush {
                Box::new(writer.take().unwrap())
            } else {
                Box::new(IdleTask)
            };
            WorkerContribution::Registration(
                WorkerRegistration::try_new(
                    WorkerId::try_new(kind, "host_test").unwrap(),
                    WorkerRunnable::Daemon {
                        restart: DaemonRestartPolicy::try_new(
                            Duration::from_secs(1),
                            Duration::from_secs(2),
                        )
                        .unwrap(),
                        task,
                    },
                )
                .unwrap(),
            )
        })
        .collect();
    host.start_workers(plan, Arc::new(UnusedLease), Arc::new(Diagnostics))
        .unwrap();
    probe
}
