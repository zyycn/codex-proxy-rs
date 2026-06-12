use reqwest::Url;

use super::{AccountService, AccountServiceError};

impl AccountService {
    pub async fn get_cookies(
        &self,
        account_id: &str,
    ) -> Result<Option<String>, AccountServiceError> {
        self.ensure_account_exists(account_id).await?;
        self.cookie_repository()?
            .cookie_header(account_id, "chatgpt.com")
            .await
            .map_err(|_| AccountServiceError::LoadCookies)
    }

    pub async fn set_cookies(
        &self,
        account_id: &str,
        cookie_header: &str,
    ) -> Result<Option<String>, AccountServiceError> {
        self.ensure_account_exists(account_id).await?;
        let cookie_repo = self.cookie_repository()?;
        match cookie_repo
            .set_cookie_header(account_id, cookie_header)
            .await
        {
            Ok(0) => Err(AccountServiceError::NoValidCookies),
            Ok(_) => cookie_repo
                .cookie_header(account_id, "chatgpt.com")
                .await
                .map_err(|_| AccountServiceError::LoadCookies),
            Err(_) => Err(AccountServiceError::StoreCookies),
        }
    }

    pub async fn delete_cookies(&self, account_id: &str) -> Result<(), AccountServiceError> {
        self.ensure_account_exists(account_id).await?;
        self.cookie_repository()?
            .delete_account_cookies(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AccountServiceError::DeleteCookies)
    }
}

pub(super) fn request_domain(base_url: &str) -> Option<String> {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
}
