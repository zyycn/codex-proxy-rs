//! 验证插件授权回调状态的一次性消费、身份绑定与过期拒绝

use gateway_admin::{
    PluginManagementService,
    model::{
        auth::{AdminPrincipal, AdminRequestContext},
        plugins::management::{PluginManagementRequest, StartPluginManagementCallback},
    },
};
use serde_json::json;

use super::{Environment, install, registration};

fn request() -> PluginManagementRequest {
    PluginManagementRequest {
        headers: vec![gateway_core::engine::middleware::MiddlewareHeader::new(
            "cookie",
            "callback=fixture".into(),
        )],
        method: "GET".into(),
        path: "oauth".into(),
        query: "code=synthetic-code".into(),
        content_type: None,
        body: Vec::new(),
        request_id: "callback-test".into(),
    }
}

#[tokio::test]
async fn public_login_callbacks_consume_bound_state_once_and_reject_wrong_owner_instance_and_expiry()
 {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let mut registration = registration();
    registration["callbacks"] = json!([{"path":"oauth","response_content_types":["text/plain"]}]);
    let first = install(
        &environment,
        json!({"management_registration":registration}),
    )
    .await;
    let second = install(
        &environment,
        json!({"management_registration":{"callbacks":registration["callbacks"]}}),
    )
    .await;
    let (runtime, core) = environment.runtime().await;
    let service = PluginManagementService::new(
        runtime.clone(),
        environment.store.admin_ports().plugins(),
        core.snapshots(),
    );
    let views = service.views().await.unwrap();
    let callback_only = views
        .iter()
        .find(|view| view.target.instance_id == second.id)
        .unwrap();
    assert!(callback_only.routes.is_empty());
    assert!(callback_only.resources.is_empty());
    assert!(callback_only.pages.is_empty());
    assert_eq!(callback_only.callbacks.len(), 1);
    let target = &views
        .iter()
        .find(|view| view.target.instance_id == first.id)
        .unwrap()
        .target;
    let other = &views
        .iter()
        .find(|view| view.target.instance_id == second.id)
        .unwrap()
        .target;
    let context = AdminRequestContext {
        principal: AdminPrincipal::Session {
            admin_user_id: "synthetic-admin".into(),
        },
        request_id: "start-callback".into(),
    };
    let ticket = service
        .start_callback(
            target,
            StartPluginManagementCallback {
                path: "oauth".into(),
                ttl_seconds: 60,
            },
            &context,
        )
        .await
        .unwrap();
    assert!(!ticket.state.contains("synthetic-admin"));
    let mut wrong_owner = ticket.state.clone();
    let tail = wrong_owner.pop().unwrap();
    wrong_owner.push(if tail == '0' { '1' } else { '0' });
    assert!(
        service
            .callback(target, &wrong_owner, request())
            .await
            .is_err()
    );
    assert!(
        service
            .callback(other, &ticket.state, request())
            .await
            .is_err()
    );
    let (one, two) = tokio::join!(
        service.callback(target, &ticket.state, request()),
        service.callback(target, &ticket.state, request())
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    let response = one.or(two).unwrap();
    assert_eq!(response.body.as_ref(), b"callback received");
    assert_eq!(response.headers.len(), 1);
    assert_eq!(response.headers[0].name(), "cookie");
    assert_eq!(response.headers[0].value().as_ref(), b"callback=fixture");
    assert!(
        service
            .callback(target, &ticket.state, request())
            .await
            .is_err()
    );
    let expired = service
        .start_callback(
            target,
            StartPluginManagementCallback {
                path: "oauth".into(),
                ttl_seconds: 1,
            },
            &context,
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(
        service
            .callback(target, &expired.state, request())
            .await
            .is_err()
    );
    assert!(
        service
            .start_callback(
                target,
                StartPluginManagementCallback {
                    path: "oauth".into(),
                    ttl_seconds: 601
                },
                &context
            )
            .await
            .is_err()
    );
    drop(service);
    drop(core);
    drop(runtime);
    environment.close().await;
}
