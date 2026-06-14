mod support;

mod admin {
    mod api_contract;
    mod api_keys_route;
    mod auth;
    mod auth_repository;
    mod login_route;
    mod logs_route;
    mod models_route;
    mod settings_route;
    mod usage_stats_route;

    mod accounts {
        mod cookies_quota;
        mod import_export;
        mod list;
        mod mutation;
        mod oauth;
    }
}
