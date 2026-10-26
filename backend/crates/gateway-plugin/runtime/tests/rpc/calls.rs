//! 验证调用表在成功、远端错误和超时的结果交付前同步撤销回调资源

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use gateway_plugin_runtime::{RpcError, RpcLimits, RpcSession};
use gateway_plugin_sdk::Stage;
use serde_json::json;

use super::session::{LifecycleCallbacks, prepared_session};

#[tokio::test]
async fn terminal_results_revoke_callback_resources_before_waking_callers() {
    for method in ["echo", "deny", "hang"] {
        let (_cache, prepared, handshake, processes) = prepared_session();
        let callbacks = Arc::new(LifecycleCallbacks::default());
        let session = RpcSession::start(
            prepared,
            handshake,
            RpcLimits::default(),
            &processes,
            callbacks.clone(),
        )
        .await
        .unwrap();
        let timeout = if method == "hang" {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(2)
        };
        let result = session
            .call(
                method,
                session.context(Stage::Request, timeout),
                json!({}),
                Vec::new(),
            )
            .await;
        match method {
            "echo" => assert!(result.is_ok()),
            "deny" => assert!(matches!(result, Err(RpcError::Remote(_)))),
            "hang" => assert!(matches!(result, Err(RpcError::Timeout))),
            _ => unreachable!(),
        }
        assert_eq!(callbacks.begun.load(Ordering::Relaxed), 1);
        assert!(
            callbacks.finished.load(Ordering::Relaxed) >= 1,
            "调用结果已经交付，资源权限必须先同步撤销：{method}"
        );
        session.shutdown(Duration::from_secs(1)).await;
    }
}
