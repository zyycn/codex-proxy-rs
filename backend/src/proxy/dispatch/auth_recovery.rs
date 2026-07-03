use std::sync::Arc;

use crate::upstream::{
    accounts::{model::AccountStatus, token_refresh::RuntimeTokenRefreshService},
    token_client::OpenAiTokenClient,
};

pub(super) fn trigger_refresh_after_auth_failure(
    token_refresh: &Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    account_id: &str,
    status: AccountStatus,
) {
    if status != AccountStatus::Expired {
        return;
    }

    let token_refresh = Arc::clone(token_refresh);
    let account_id = account_id.to_string();
    tokio::spawn(async move {
        if let Err(error) = token_refresh.trigger_account_refresh_now(&account_id).await {
            tracing::warn!(
                account_id = %account_id,
                error = %error,
                "reactive token refresh after upstream auth failure failed"
            );
        }
    });
}
