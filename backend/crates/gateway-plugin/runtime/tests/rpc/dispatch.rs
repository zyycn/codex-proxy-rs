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
use gateway_plugin_sdk::{CallContext, ErrorCode, Handshake, Permission, PluginFault, Stage};
use serde_json::json;

const CALLBACK_PERMISSIONS: [(&str, Permission); 8] = [
    ("host.http.do", Permission::Network),
    ("host.model.execute", Permission::Models),
    ("host.auth.list", Permission::Accounts),
    ("host.auth.get", Permission::Accounts),
    ("host.auth.save", Permission::Accounts),
    ("host.affinity.lookup", Permission::Requests),
    ("host.models.list", Permission::Models),
    ("host.keys.list", Permission::Models),
];

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
    session_with_permissions(callbacks, Permission::ALL.to_vec()).await
}

async fn session_with_permissions(
    callbacks: Arc<Callbacks>,
    permissions: Vec<Permission>,
) -> (tempfile::TempDir, Arc<RpcSession>) {
    let cache = tempfile::tempdir().unwrap();
    let package = Arc::new(
        ValidatedPackage::read(
            crate::support::package_with_permissions(crate::support::worker(), permissions.clone()),
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
        permissions,
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

fn assert_permission_denied(reply: Result<RpcReply, RpcError>) {
    assert!(matches!(
        reply,
        Err(RpcError::Remote(PluginFault {
            code: ErrorCode::PermissionDenied,
            ..
        }))
    ));
}

#[tokio::test]
async fn public_management_cannot_inherit_any_granted_host_callback_permission() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    for (method, _) in CALLBACK_PERMISSIONS {
        assert_permission_denied(invoke_callback(&session, Stage::PublicManagement, method).await);
    }
    for method in ["host.log", "host.state.get"] {
        assert_permission_denied(invoke_callback(&session, Stage::PublicManagement, method).await);
    }
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 0);
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn registration_and_configuration_reject_network_and_model_callbacks() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;

    for stage in [Stage::Registration, Stage::Configuration] {
        assert_permission_denied(invoke_callback(&session, stage, "host.http.do").await);
        assert_permission_denied(invoke_callback(&session, stage, "host.model.execute").await);
    }
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 0);

    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn active_stages_use_resource_domains_without_operation_whitelists() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;

    for stage in [
        Stage::Routing,
        Stage::Scheduling,
        Stage::Request,
        Stage::Attempt,
        Stage::Observation,
        Stage::Authentication,
        Stage::Management,
        Stage::CommandLine,
    ] {
        for (method, _) in CALLBACK_PERMISSIONS {
            assert!(
                invoke_callback(&session, stage, method).await.is_ok(),
                "{method} domain should be available during {stage:?}"
            );
        }
    }
    assert_eq!(
        callbacks.called.load(Ordering::Relaxed),
        8 * CALLBACK_PERMISSIONS.len()
    );

    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn missing_domains_are_denied_before_dispatch() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session_with_permissions(Arc::clone(&callbacks), vec![]).await;

    for (method, _) in CALLBACK_PERMISSIONS {
        assert_permission_denied(invoke_callback(&session, Stage::Observation, method).await);
    }
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 0);

    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn logs_and_own_state_are_infrastructure_without_permissions() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session_with_permissions(Arc::clone(&callbacks), vec![]).await;

    for stage in [
        Stage::Registration,
        Stage::Configuration,
        Stage::Observation,
        Stage::Management,
        Stage::Authentication,
    ] {
        assert!(invoke_callback(&session, stage, "host.log").await.is_ok());
        if matches!(stage, Stage::Registration | Stage::Configuration) {
            assert_permission_denied(invoke_callback(&session, stage, "host.state.get").await);
        } else {
            assert!(
                invoke_callback(&session, stage, "host.state.get")
                    .await
                    .is_ok()
            );
        }
    }
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 8);

    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn execution_allows_each_declared_callback_permission() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;

    for (method, _) in CALLBACK_PERMISSIONS {
        let reply = invoke_callback(&session, Stage::Request, method).await;
        assert!(reply.is_ok(), "{method} should be allowed during execution");
    }
    assert_eq!(
        callbacks.called.load(Ordering::Relaxed),
        CALLBACK_PERMISSIONS.len()
    );

    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn data_callbacks_require_independent_grants_and_management_or_command_stage() {
    for permissions in [vec![Permission::Accounts], vec![Permission::Data]] {
        let authorized = permissions.contains(&Permission::Data);
        let callbacks = Arc::new(Callbacks::default());
        let (_cache, session) = session_with_permissions(Arc::clone(&callbacks), permissions).await;
        for stage in [
            Stage::Registration,
            Stage::Configuration,
            Stage::Routing,
            Stage::Scheduling,
            Stage::Request,
            Stage::Attempt,
            Stage::Retry,
            Stage::Observation,
            Stage::Authentication,
            Stage::PublicManagement,
            Stage::Management,
            Stage::CommandLine,
        ] {
            for method in ["host.data.accounts.list", "host.data.quota.get"] {
                let reply = invoke_callback(&session, stage, method).await;
                if authorized && matches!(stage, Stage::Management | Stage::CommandLine) {
                    assert!(reply.is_ok());
                } else {
                    assert_permission_denied(reply);
                }
            }
        }
        assert_eq!(
            callbacks.called.load(Ordering::Relaxed),
            if authorized { 4 } else { 0 }
        );
        session.shutdown(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn retry_stage_allows_only_logs_and_private_state() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(Arc::clone(&callbacks)).await;
    for (method, _) in CALLBACK_PERMISSIONS {
        assert_permission_denied(invoke_callback(&session, Stage::Retry, method).await);
    }
    for method in ["host.log", "host.state.get"] {
        assert!(
            invoke_callback(&session, Stage::Retry, method)
                .await
                .is_ok()
        );
    }
    assert_eq!(callbacks.called.load(Ordering::Relaxed), 2);
    session.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn managed_resources_require_their_domains_and_control_plane_stages() {
    for (method, permission) in [
        ("host.groups.ensure", Permission::Groups),
        ("host.groups.change_members", Permission::Groups),
        ("host.keys.ensure", Permission::Keys),
    ] {
        for granted in [false, true] {
            let callbacks = Arc::new(Callbacks::default());
            let (_cache, session) = session_with_permissions(
                callbacks.clone(),
                if granted { vec![permission] } else { vec![] },
            )
            .await;
            for stage in [
                Stage::Registration,
                Stage::Configuration,
                Stage::PublicManagement,
                Stage::Request,
                Stage::Observation,
                Stage::Management,
                Stage::CommandLine,
                Stage::Maintenance,
            ] {
                let reply = invoke_callback(&session, stage, method).await;
                if granted
                    && matches!(
                        stage,
                        Stage::Management | Stage::CommandLine | Stage::Maintenance
                    )
                {
                    assert!(reply.is_ok());
                } else {
                    assert_permission_denied(reply);
                }
            }
            session.shutdown(Duration::from_secs(1)).await;
        }
    }
}

#[tokio::test]
async fn maintenance_has_data_and_state_access_but_cannot_execute_models_or_read_credentials() {
    let callbacks = Arc::new(Callbacks::default());
    let (_cache, session) = session(callbacks).await;
    for method in [
        "host.data.accounts.list",
        "host.data.quota.get",
        "host.state.put",
        "host.log",
    ] {
        assert!(
            invoke_callback(&session, Stage::Maintenance, method)
                .await
                .is_ok()
        );
    }
    for method in [
        "host.auth.get",
        "host.auth.save",
        "host.http.do",
        "host.model.execute",
        "host.keys.list",
    ] {
        assert_permission_denied(invoke_callback(&session, Stage::Maintenance, method).await);
    }
    session.shutdown(Duration::from_secs(1)).await;
}
