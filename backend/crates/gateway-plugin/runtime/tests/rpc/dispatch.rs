//! 验证插件各调用阶段的宿主回调分派

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use gateway_plugin_runtime::{
    CallbackHandler, PackageLimits, RpcError, RpcLimits, RpcReply, RpcSession, ValidatedPackage,
};
use gateway_plugin_sdk::{CallContext, Handshake, PluginFault, Stage};
use serde_json::json;

#[derive(Default)]
struct Callbacks {
    called: AtomicUsize,
}

impl CallbackHandler for Callbacks {
    fn call(
        &self,
        _context: CallContext,
        _method: String,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> BoxFuture<'static, Result<RpcReply, PluginFault>> {
        self.called.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            Ok(RpcReply {
                result: params,
                payload,
            })
        })
    }
}

async fn session(callbacks: Arc<Callbacks>) -> (tempfile::TempDir, Arc<RpcSession>) {
    let cache = tempfile::tempdir().unwrap();
    let package = Arc::new(
        ValidatedPackage::read(
            crate::support::package(crate::support::worker()),
            None,
            PackageLimits::default(),
        )
        .unwrap(),
    );
    let prepared = Arc::new(
        package
            .prepare(cache.path(), &"1.0.0".parse().unwrap())
            .unwrap(),
    );
    let handshake = Handshake {
        protocol_version: gateway_plugin_sdk::PROTOCOL_VERSION,
        artifact_sha256: package.digest().into(),
        plugin_id: package.manifest().plugin_id().unwrap(),
        instance_id: "test-instance".into(),
        generation: 1,
        incarnation: uuid::Uuid::new_v4().to_string(),
        configuration: json!({}),
        contributes: gateway_plugin_sdk::Contributions::new(),
    };
    let processes =
        gateway_host::process::ProcessSupervisor::new(std::num::NonZeroUsize::new(128).unwrap());
    let session = RpcSession::start(
        prepared,
        handshake,
        RpcLimits::default(),
        &processes,
        callbacks,
    )
    .await
    .unwrap();
    (cache, Arc::new(session))
}

async fn invoke_callback(
    session: &RpcSession,
    stage: Stage,
    method: &str,
) -> Result<RpcReply, RpcError> {
    session
        .call(
            "callback_method",
            session.context(stage, Duration::from_secs(2)),
            json!({"method": method}),
            vec![],
        )
        .await
}

#[tokio::test]
async fn callbacks_are_dispatched_in_every_stage_without_permission_declarations() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    let methods = [
        "host.http.do",
        "host.model.execute",
        "host.auth.list",
        "host.auth.get",
        "host.auth.save",
        "host.affinity.lookup",
        "host.models.list",
        "host.keys.list",
        "host.log",
        "host.state.get",
        "host.state.put",
        "host.data.accounts.list",
        "host.data.keys.get",
        "host.data.quota.get",
        "host.groups.ensure",
        "host.groups.change_members",
        "host.keys.ensure",
        "host.keys.reset_budget",
        "host.keys.get_budget",
        "host.keys.update_budget_limits",
        "host.quota_observations.refresh",
        "host.upstream.http.do",
        "host.upstream.http.do_stream",
        "host.upstream.http.stream_read",
        "host.upstream.http.stream_close",
        "host.upstream.websocket.open",
        "host.upstream.websocket.send",
        "host.upstream.websocket.read",
        "host.upstream.websocket.close",
    ];
    let stages = [
        Stage::Registration,
        Stage::Configuration,
        Stage::Authentication,
        Stage::Routing,
        Stage::Scheduling,
        Stage::Retry,
        Stage::Request,
        Stage::Attempt,
        Stage::Upstream,
        Stage::Observation,
        Stage::Management,
        Stage::CommandLine,
        Stage::PublicManagement,
        Stage::Maintenance,
    ];
    for stage in stages {
        for method in methods {
            assert!(
                invoke_callback(&session, stage, method).await.is_ok(),
                "{stage:?}: {method}"
            );
        }
    }
    assert_eq!(
        callbacks.called.load(Ordering::Relaxed),
        methods.len() * stages.len()
    );
    session.shutdown(Duration::from_secs(1)).await;
}
