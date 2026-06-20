use super::*;

/// 管理端模型服务。
#[derive(Clone)]
pub struct AdminModelService {
    models: Arc<ModelService>,
    accounts: Arc<dyn AccountStore>,
    installation_id: Option<String>,
}

impl AdminModelService {
    /// 构造管理端模型服务。
    pub fn new(
        models: Arc<ModelService>,
        accounts: Arc<dyn AccountStore>,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            models,
            accounts,
            installation_id,
        }
    }

    /// 用当前活跃账号刷新上游模型快照。
    pub async fn refresh_backend_models(
        &self,
        request_id: &str,
    ) -> Result<ModelRefreshResult, AdminModelError> {
        let accounts = self
            .accounts
            .list_pool_accounts()
            .await
            .map_err(|_| AdminModelError::ListAccounts)?;
        self.models
            .refresh_backend_models_with_installation_id(
                &accounts,
                request_id,
                self.installation_id.as_deref(),
            )
            .await
            .map_err(AdminModelError::from)
    }
}

/// 管理端模型错误。
#[derive(Debug, Error)]
pub enum AdminModelError {
    /// 列出账号失败。
    #[error("failed to list accounts")]
    ListAccounts,
    /// 没有可用账号。
    #[error("no active accounts available for model refresh")]
    NoAccounts,
    /// 模型快照存储不可用。
    #[error("model snapshot store is unavailable")]
    SnapshotStoreUnavailable,
    /// 上游客户端不可用。
    #[error("model upstream client is unavailable")]
    UpstreamClientUnavailable,
    /// 写入快照失败。
    #[error("failed to store model snapshot")]
    StoreSnapshot,
    /// 读取快照失败。
    #[error("failed to load model snapshots")]
    LoadSnapshots,
    /// 所有计划刷新失败。
    #[error("all model refresh plans failed")]
    AllPlansFailed(ModelRefreshResult),
}

impl From<ModelServiceError> for AdminModelError {
    fn from(error: ModelServiceError) -> Self {
        match error {
            ModelServiceError::SnapshotStoreUnavailable => Self::SnapshotStoreUnavailable,
            ModelServiceError::UpstreamClientUnavailable => Self::UpstreamClientUnavailable,
            ModelServiceError::NoAccounts => Self::NoAccounts,
            ModelServiceError::StoreSnapshot => Self::StoreSnapshot,
            ModelServiceError::LoadSnapshots => Self::LoadSnapshots,
            ModelServiceError::AllPlansFailed(result) => Self::AllPlansFailed(result),
        }
    }
}
