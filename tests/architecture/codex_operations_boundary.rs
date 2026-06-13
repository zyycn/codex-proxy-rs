use codex_proxy_rs::codex::{
    logs::{event::EventLog, repository::EventLogRepository, service::LogService},
    tasks::{model::ModelRefresher, quota::QuotaRefresher, refresh::RefreshScheduler},
    usage::service::UsageService,
};

#[test]
fn codex_exports_operations_modules() {
    let _event_type = std::any::type_name::<EventLog>();
    let _event_repo_type = std::any::type_name::<EventLogRepository>();
    let _log_service_type = std::any::type_name::<LogService>();
    let _usage_service_type = std::any::type_name::<UsageService>();
    let _refresh_scheduler_type = std::any::type_name::<RefreshScheduler>();
    let _quota_refresher_type = std::any::type_name::<QuotaRefresher>();
    let _model_refresher_type = std::any::type_name::<ModelRefresher>();
}
