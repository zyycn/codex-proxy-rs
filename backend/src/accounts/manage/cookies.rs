use super::AccountManageService;

impl AccountManageService {
    pub(crate) async fn usage_cookie_header(&self, account_id: &str) -> Option<String> {
        self.cookies
            .cookie_header_for_request(account_id, "chatgpt.com", "/codex/usage")
            .await
            .ok()
            .flatten()
    }
}
