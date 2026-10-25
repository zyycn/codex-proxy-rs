//! 插件发布准备各职责的集成测试入口

mod diagnostics;
mod instance;
mod maintenance;
mod management;
mod runtime;
mod set;
mod snapshot;

use std::time::Duration;

async fn wait_until_unready(reference: &gateway_core::routing::extensions::ExtensionSetReference) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while reference.is_ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker exit observed");
}
