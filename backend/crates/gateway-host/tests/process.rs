//! 验证真实子进程的启动、停止、回收与容量释放

#[cfg(unix)]
use gateway_host::process::{ProcessExit, ProcessSpec, ProcessStartError, ProcessSupervisor};

#[cfg(unix)]
fn worker(directory: &std::path::Path, mode: &str) -> ProcessSpec {
    let executable = env!("CARGO_BIN_EXE_gateway-host-process-worker").into();
    std::fs::write(directory.join("mode"), mode).unwrap();
    ProcessSpec {
        executable,
        directory: directory.to_owned(),
        maximum_stderr_bytes: 128,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_a_real_child_waits_for_reaping() {
    let directory = tempfile::tempdir().unwrap();
    let supervisor = ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
    let process = supervisor.spawn(worker(directory.path(), "wait")).unwrap();
    assert!(matches!(
        supervisor.spawn(worker(directory.path(), "wait")),
        Err(ProcessStartError::Capacity)
    ));
    process.control.stop();
    let exit = tokio::time::timeout(std::time::Duration::from_secs(3), process.control.exited())
        .await
        .unwrap();
    assert_eq!(exit, ProcessExit::Stopped);
    let replacement = supervisor.spawn(worker(directory.path(), "wait")).unwrap();
    replacement.control.stop();
    assert_eq!(replacement.control.exited().await, ProcessExit::Stopped);
}

#[cfg(unix)]
#[tokio::test]
async fn unbounded_stderr_is_stopped_without_retaining_its_content() {
    let directory = tempfile::tempdir().unwrap();
    let supervisor = ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
    let process = supervisor
        .spawn(worker(directory.path(), "stderr"))
        .unwrap();
    let exit = tokio::time::timeout(std::time::Duration::from_secs(3), process.control.exited())
        .await
        .unwrap();
    assert_eq!(exit, ProcessExit::StderrLimit);
}

#[cfg(unix)]
#[tokio::test]
async fn failed_spawn_preserves_safe_cause_and_returns_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let supervisor = ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
    let mut missing = worker(directory.path(), "wait");
    missing.executable = directory.path().join("private-test-only-executable");
    let error = supervisor.spawn(missing).err().unwrap();
    assert!(matches!(
        error,
        ProcessStartError::Spawn {
            kind: std::io::ErrorKind::NotFound,
            os_code: Some(_),
        }
    ));
    assert!(!format!("{error:?} {error}").contains("private-test-only-executable"));
    let process = supervisor.spawn(worker(directory.path(), "wait")).unwrap();
    process.control.stop();
    assert_eq!(process.control.exited().await, ProcessExit::Stopped);
}

#[cfg(unix)]
#[tokio::test]
async fn busy_and_blocked_children_can_be_stopped_and_reaped() {
    use tokio::io::AsyncReadExt as _;

    for mode in ["busy", "stdin", "stdout"] {
        let directory = tempfile::tempdir().unwrap();
        let supervisor = ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
        let mut process = supervisor.spawn(worker(directory.path(), mode)).unwrap();
        let ready =
            tokio::time::timeout(std::time::Duration::from_secs(3), process.output.read_u8())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(ready, b'R');
        // 就绪后保留输入句柄且不再读取输出，确保测试的是运行中的死循环或阻塞 I/O
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!directory.path().join("unblocked").exists());
        process.control.stop();
        let exit =
            tokio::time::timeout(std::time::Duration::from_secs(3), process.control.exited())
                .await
                .unwrap();
        assert_eq!(exit, ProcessExit::Stopped, "mode: {mode}");
        let replacement = supervisor.spawn(worker(directory.path(), "wait")).unwrap();
        replacement.control.stop();
        assert_eq!(replacement.control.exited().await, ProcessExit::Stopped);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn abnormal_exit_is_observed_and_returns_process_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let supervisor = ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
    let process = supervisor.spawn(worker(directory.path(), "exit")).unwrap();
    let exit = tokio::time::timeout(std::time::Duration::from_secs(3), process.control.exited())
        .await
        .unwrap();
    assert_eq!(exit, ProcessExit::Exited(Some(23)));
    let replacement = supervisor.spawn(worker(directory.path(), "wait")).unwrap();
    replacement.control.stop();
    assert_eq!(replacement.control.exited().await, ProcessExit::Stopped);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn allocation_failure_in_a_bounded_child_returns_process_capacity() {
    use std::os::unix::fs::PermissionsExt as _;
    use tokio::io::AsyncReadExt as _;

    let directory = tempfile::tempdir().unwrap();
    let supervisor = ProcessSupervisor::new(std::num::NonZeroUsize::new(1).unwrap());
    let mut spec = worker(directory.path(), "allocate");
    let executable = spec.executable.to_str().unwrap().replace('\'', "'\\''");
    let wrapper = directory.path().join("bounded-worker");
    // 地址空间上限和禁用 core dump 仅保护测试机，不宣称生产监督器已施加 OS 内存隔离
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nset -eu\nulimit -c 0\nulimit -v 65536\nexec '{executable}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    spec.executable = wrapper;
    let mut process = supervisor.spawn(spec).unwrap();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(3), process.output.read_u8())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready, b'R');
    let exit = tokio::time::timeout(std::time::Duration::from_secs(3), process.control.exited())
        .await
        .unwrap();
    assert_eq!(exit, ProcessExit::Exited(None));
    assert!(!directory.path().join("allocation-succeeded").exists());
    let replacement = supervisor.spawn(worker(directory.path(), "wait")).unwrap();
    replacement.control.stop();
    assert_eq!(replacement.control.exited().await, ProcessExit::Stopped);
}
