use codex_proxy_core::{
    events::{model::EventLog, service::EventLogService},
    usage::service::UsageService,
};
use codex_proxy_runtime::tasks::model_refresh::ModelRefreshTask;

#[test]
fn codex_exports_activity_modules() {
    let _event_type = std::any::type_name::<EventLog>();
    let _log_service_type = std::any::type_name::<EventLogService>();
    let _usage_service_type = std::any::type_name::<UsageService>();
    let _model_refresher_type = std::any::type_name::<ModelRefreshTask>();
}
