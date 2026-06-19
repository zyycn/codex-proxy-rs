use codex_proxy_runtime::tasks::coordinator::{
    start_background_tasks, BackgroundTaskCoordinator, SchedulerHandle,
};

#[test]
fn runtime_exports_background_task_coordinator_modules() {
    let _coordinator_type = std::any::type_name::<BackgroundTaskCoordinator>();
    let _handle_type = std::any::type_name::<SchedulerHandle>();
    let _start_fn = start_background_tasks;
}
