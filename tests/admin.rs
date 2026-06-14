mod support;

mod admin {
    mod api_contract;
    mod client_keys_route;
    mod logs_route;
    mod models_route;
    mod session;
    mod session_login_route;
    mod session_repository;
    mod settings_route;
    mod usage_stats_route;

    mod accounts {
        mod cookies_quota;
        mod import_export;
        mod lifecycle;
        mod list;
        mod oauth;
    }
}
