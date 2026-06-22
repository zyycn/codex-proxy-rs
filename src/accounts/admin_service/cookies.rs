use super::{types::*, AdminAccountService};

impl AdminAccountService {
    pub async fn cookies(&self, account_id: &str) -> Result<Option<String>, AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .cookie_header(account_id, "chatgpt.com")
            .await
            .map_err(|_| AdminAccountError::LoadCookies)
    }
    pub async fn set_cookies(
        &self,
        account_id: &str,
        cookies: serde_json::Value,
    ) -> Result<Option<String>, AdminAccountError> {
        let cookie_header = match cookies {
            serde_json::Value::String(ref s) => s.trim().to_string(),
            serde_json::Value::Object(ref obj) => {
                let pairs: Vec<String> = obj
                    .iter()
                    .filter_map(|(name, val)| {
                        let v = val.as_str()?.trim();
                        if name.trim().is_empty() || v.is_empty() {
                            return None;
                        }
                        Some(format!("{}={}", name.trim(), v))
                    })
                    .collect();
                if pairs.is_empty() {
                    return Err(AdminAccountError::NoValidCookies);
                }
                pairs.join("; ")
            }
            _ => return Err(AdminAccountError::NoValidCookies),
        };
        self.ensure_cookie_account_exists(account_id).await?;
        match self
            .cookies
            .set_cookie_header(account_id, &cookie_header)
            .await
        {
            Ok(0) => Err(AdminAccountError::NoValidCookies),
            Ok(_) => self
                .cookies
                .cookie_header(account_id, "chatgpt.com")
                .await
                .map_err(|_| AdminAccountError::LoadCookies),
            Err(_) => Err(AdminAccountError::StoreCookies),
        }
    }
    pub async fn delete_cookies(&self, account_id: &str) -> Result<(), AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .delete_account_cookies(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::DeleteCookies)
    }
    async fn ensure_cookie_account_exists(
        &self,
        account_id: &str,
    ) -> Result<(), AdminAccountError> {
        match self.cookies.account_exists(account_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AdminAccountError::NotFound),
            Err(_) => Err(AdminAccountError::Inspect),
        }
    }
}
