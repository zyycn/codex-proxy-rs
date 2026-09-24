fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // 测试插件会反复装包和解包；调试段不参与协议行为，不能让它放大每个用例的 I/O。
    // 仅调整两个测试子进程，宿主和测试入口仍保留完整调试信息。
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "linux" | "macos") {
        for binary in [
            "gateway-plugin-test-worker",
            "gateway-plugin-test-middleware",
        ] {
            println!("cargo::rustc-link-arg-bin={binary}=-Wl,-S");
        }
    }
}
