use crate::{
    codex::accounts::repository::{
        AccountRepositoryError, AccountUsageListRecord, AccountUsageRepository, AccountUsageSummary,
    },
    utils::pagination::Page,
};

#[derive(Clone)]
pub struct UsageService {
    repository: Option<AccountUsageRepository>,
}

#[derive(Debug)]
pub enum UsageServiceError {
    RepositoryUnavailable,
    Repository(AccountRepositoryError),
}

impl UsageService {
    pub fn new(repository: Option<AccountUsageRepository>) -> Self {
        Self { repository }
    }

    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AccountUsageListRecord>, UsageServiceError> {
        self.repository()?
            .list(cursor, limit)
            .await
            .map_err(UsageServiceError::Repository)
    }

    pub async fn summary(&self) -> Result<AccountUsageSummary, UsageServiceError> {
        self.repository()?
            .summary()
            .await
            .map_err(UsageServiceError::Repository)
    }

    fn repository(&self) -> Result<&AccountUsageRepository, UsageServiceError> {
        self.repository
            .as_ref()
            .ok_or(UsageServiceError::RepositoryUnavailable)
    }
}
