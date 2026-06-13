use codex_proxy_rs::codex::serving::{
    chat::ChatService, diagnostics::DiagnosticsService, dispatch::affinity::SessionAffinityMap,
    http::router::router, responses::ResponsesService,
};

#[test]
fn serving_exports_client_facing_proxy_modules() {
    let _chat_type = std::any::type_name::<ChatService>();
    let _responses_type = std::any::type_name::<ResponsesService>();
    let _diagnostics_type = std::any::type_name::<DiagnosticsService>();
    let _affinity_type = std::any::type_name::<SessionAffinityMap>();
    let _router_fn = router;
}
