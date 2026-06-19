use codex_proxy_core::admin::{
    auth::AdminAuthService, client_keys::ClientKeyService, settings::SettingsService,
};
use codex_proxy_server::admin_api::router::router;

#[test]
fn admin_exports_control_plane_modules() {
    let _admin_auth_type = std::any::type_name::<AdminAuthService>();
    let _api_key_type = std::any::type_name::<ClientKeyService>();
    let _settings_type = std::any::type_name::<SettingsService>();
    let _router_fn = router;
}
