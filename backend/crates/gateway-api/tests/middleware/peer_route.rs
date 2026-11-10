//! 多字段路径、跨协议拒绝与长链路的入口合同

use super::*;
use gateway_protocol::peer::{ROUTE_HEADER, ROUTING_LOOP_CODE};

#[tokio::test]
async fn peer_route_enters_once_and_only_rejects_loops_or_malformed_paths() {
    let (app, plan) = router(Mode::PeerRoute, None).await;
    let path = (1..=16).map(|id| format!("{id:032x}")).collect::<Vec<_>>();
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/responses")
                .header(ROUTE_HEADER, path[..8].join(", "))
                .header(ROUTE_HEADER, path[8..].join(","))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let local = uuid::Uuid::nil().simple().to_string();
    assert_eq!(
        response.headers()["test-route"],
        format!("{},{}", path.join(","), local)
    );
    for (route, status, code) in [
        (
            format!("{},{local},{}", path[3], path[2]),
            StatusCode::LOOP_DETECTED,
            ROUTING_LOOP_CODE,
        ),
        (
            "bad-id".into(),
            StatusCode::BAD_REQUEST,
            "invalid_peer_route",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/messages")
                    .header(ROUTE_HEADER, route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let body = crate::support::response_json(response).await;
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["code"], code);
    }
    assert_eq!(plan.calls.load(Ordering::SeqCst), 1);
}
