//! 上游认证失败分类。

use crate::{
    fleet::account::AccountStatus,
    upstream::openai::transport::{is_banned_auth_signal, CodexClientError},
};

pub(crate) fn is_auth_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status.as_u16() == 401
    )
}

pub(crate) fn auth_failure_account_status(error: &CodexClientError) -> AccountStatus {
    match error {
        CodexClientError::Upstream { body, .. } if is_banned_auth_signal(body) => {
            AccountStatus::Banned
        }
        _ => AccountStatus::Expired,
    }
}
