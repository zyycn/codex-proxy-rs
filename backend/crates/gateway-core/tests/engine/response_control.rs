//! 验证原连接控制端口的透明传递、owner 生命周期与执行隔离

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{FutureExt, executor::block_on};
use gateway_core::engine::response_control::{
    ResponseControl, ResponseControlTransport, ResponseControlUnavailable,
};

#[derive(Default)]
struct Transport(Mutex<Vec<String>>);

#[async_trait]
impl ResponseControlTransport for Transport {
    async fn send(&self, payload: &str) -> Result<(), ResponseControlUnavailable> {
        self.0.lock().unwrap().push(payload.to_owned());
        Ok(())
    }
    async fn receive(&self) -> Result<String, ResponseControlUnavailable> {
        Ok("upstream reply".to_owned())
    }
}

#[test]
fn control_requires_a_live_connection_owner_and_preserves_unknown_payloads() {
    block_on(async {
        let control = ResponseControl::default();
        assert_eq!(
            control.send("unknown").await,
            Err(ResponseControlUnavailable)
        );
        assert!(control.receive().now_or_never().is_none());
        let transport = Arc::new(Transport::default());
        let owner: Arc<dyn ResponseControlTransport> = transport.clone();
        control.bind(&owner);
        let payload = "{ \"type\": \"future.control\", \"extension\": [1, 2] }";
        control.send(payload).await.unwrap();
        control.send(payload).await.unwrap();
        assert_eq!(*transport.0.lock().unwrap(), [payload, payload]);
        assert_eq!(control.receive().await.unwrap(), "upstream reply");
        drop(owner);
        drop(transport);
        assert_eq!(control.send(payload).await, Err(ResponseControlUnavailable));
    });
}

#[test]
fn replacing_or_clearing_a_binding_does_not_share_control_between_executions() {
    block_on(async {
        let first = ResponseControl::default();
        let second = ResponseControl::default();
        let old: Arc<dyn ResponseControlTransport> = Arc::new(Transport::default());
        let current = Arc::new(Transport::default());
        let owner: Arc<dyn ResponseControlTransport> = current.clone();
        first.bind(&old);
        second.bind(&owner);
        drop(old);
        assert_eq!(first.send("old").await, Err(ResponseControlUnavailable));
        second.send("current").await.unwrap();
        assert_eq!(*current.0.lock().unwrap(), ["current"]);
        second.clear();
        assert_eq!(
            second.send("cleared").await,
            Err(ResponseControlUnavailable)
        );
    });
}

#[test]
fn captured_binding_never_redirects_old_control_to_a_replacement() {
    block_on(async {
        let control = ResponseControl::default();
        let old = Arc::new(Transport::default());
        let old_owner: Arc<dyn ResponseControlTransport> = old.clone();
        control.bind(&old_owner);
        let binding = control.binding().unwrap();
        let new = Arc::new(Transport::default());
        let new_owner: Arc<dyn ResponseControlTransport> = new.clone();
        control.bind(&new_owner);
        assert!(!binding.same_binding(&control.binding().unwrap()));
        binding.send("old ID").await.unwrap();
        assert_eq!(*old.0.lock().unwrap(), ["old ID"]);
        assert!(new.0.lock().unwrap().is_empty());
        drop(old_owner);
        drop(old);
        assert_eq!(
            binding.send("expired ID").await,
            Err(ResponseControlUnavailable)
        );
        assert!(new.0.lock().unwrap().is_empty());
    });
}
