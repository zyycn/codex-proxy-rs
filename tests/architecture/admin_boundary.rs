use codex_proxy_rs::admin::{
    api::router::router, client_keys::service::ApiKeyService, session::service::AdminAuthService,
    settings::SettingsService, tasks::session_cleanup::SessionCleanupScheduler,
};

#[test]
fn admin_exports_control_plane_modules() {
    let _admin_auth_type = std::any::type_name::<AdminAuthService>();
    let _api_key_type = std::any::type_name::<ApiKeyService>();
    let _settings_type = std::any::type_name::<SettingsService>();
    let _session_cleanup_type = std::any::type_name::<SessionCleanupScheduler>();
    let _router_fn = router;
}
