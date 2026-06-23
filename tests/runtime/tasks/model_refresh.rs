use super::*;

#[tokio::test]
async fn model_refresh_task_should_start_and_shutdown() {
    let model_service = Arc::new(ModelService::new(
        ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: Default::default(),
        },
        None,
        None,
        None,
    ));
    let account_store = Arc::new(FakeAccountStore);

    let handle = codex_proxy_rs::runtime::tasks::model_refresh::ModelRefreshTask::new(
        model_service,
        account_store,
    )
    .start();

    handle.shutdown().await;
}
