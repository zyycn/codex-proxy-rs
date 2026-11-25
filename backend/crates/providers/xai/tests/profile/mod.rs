//! 测试模块入口与共享夹具

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use futures::future::BoxFuture;
use provider_xai::{
    GrokCliReleaseError, GrokCliReleaseService, GrokCliReleaseTransport, XaiWireProfile,
    XaiWireProfileState,
};

struct ReleaseTransport {
    outcomes: Mutex<VecDeque<Result<String, GrokCliReleaseError>>>,
}

impl ReleaseTransport {
    fn new(outcomes: impl IntoIterator<Item = Result<String, GrokCliReleaseError>>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
}

impl GrokCliReleaseTransport for ReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<String, GrokCliReleaseError>> {
        let outcome = self
            .outcomes
            .lock()
            .expect("release outcomes lock")
            .pop_front()
            .expect("release outcome");
        Box::pin(async move { outcome })
    }
}

fn wire_profile() -> XaiWireProfileState {
    XaiWireProfileState::new(XaiWireProfile {
        client_identifier: "grok-shell".to_owned(),
        client_version: "0.2.106".to_owned(),
        client_mode: "headless".to_owned(),
        target_os: "linux".to_owned(),
        target_arch: "x86_64".to_owned(),
        verified_at: Utc::now(),
    })
}

mod selection;

mod state;

mod release;
