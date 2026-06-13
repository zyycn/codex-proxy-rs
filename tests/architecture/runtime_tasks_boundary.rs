use codex_proxy_rs::runtime::tasks::{
    coordinator::{start_background_tasks, BackgroundTaskCoordinator},
    types::{SchedulerError, SchedulerHandle},
};

#[test]
fn runtime_exports_background_task_coordinator_modules() {
    let _coordinator_type = std::any::type_name::<BackgroundTaskCoordinator>();
    let _handle_type = std::any::type_name::<SchedulerHandle>();
    let _error_type = std::any::type_name::<SchedulerError>();
    let _start_fn = start_background_tasks;
}
