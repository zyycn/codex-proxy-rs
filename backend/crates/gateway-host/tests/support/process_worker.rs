//! Host 进程监督测试的 Rust 子进程，不依赖脚本解释器

use std::{
    io::{Read as _, Write as _},
    time::Duration,
};

fn main() {
    let executable = std::env::current_exe().unwrap();
    let directory = executable.parent().unwrap();
    if directory.join("restart-probe").is_file() {
        std::fs::write(directory.join("replacement-waiting"), b"ready").unwrap();
        let arguments = gateway_host::system_update::startup_arguments()
            .unwrap()
            .collect::<Vec<_>>();
        assert!(!arguments.iter().any(|value| value == "--restart-handoff"));
        assert!(!directory.join("update.lock").exists());
        std::fs::write(directory.join("replacement-ran"), b"started").unwrap();
        return;
    }
    let mode = std::fs::read_to_string("mode").unwrap();
    if mode == "stderr" {
        std::io::stderr().write_all(&[b'x'; 4096]).unwrap();
    }
    if matches!(
        mode.as_str(),
        "busy" | "stdin" | "stdout" | "exit" | "allocate" | "update-probe"
    ) {
        std::io::stdout().write_all(b"R").unwrap();
        std::io::stdout().flush().unwrap();
    }
    match mode.as_str() {
        "busy" => loop {
            std::hint::spin_loop();
        },
        "stdin" => {
            std::io::stdin().read_exact(&mut [0; 1]).unwrap();
            std::fs::write("unblocked", b"stdin").unwrap();
        }
        "update-probe" => {
            std::io::stdin().read_exact(&mut [0; 1]).unwrap();
            std::fs::write("unblocked", b"still-running").unwrap();
            return;
        }
        "stdout" => {
            // 测试端不消费此正文，超过管道容量后必须由监督器终止，而不是等待写入完成
            std::io::stdout()
                .write_all(&vec![b'x'; 8 * 1024 * 1024])
                .unwrap();
            std::fs::write("unblocked", b"stdout").unwrap();
        }
        "exit" => std::process::exit(23),
        "allocate" => {
            // 仅供测试包装器在严格地址空间上限内触发真实分配失败，不能无保护地运行此模式
            std::hint::black_box(vec![b'x'; 64 * 1024 * 1024]);
            std::fs::write("allocation-succeeded", b"unexpected").unwrap();
        }
        _ => {}
    }
    std::thread::sleep(Duration::from_secs(60));
}
