//! 客户端身份、本地会话索引与账号作用域 wire 身份隔离。

use crate::{
    infra::identity::AccountPseudonymizer,
    upstream::openai::protocol::responses::{CodexCompactRequest, CodexResponsesRequest},
};

/// 持久化密钥驱动的账号身份隔离服务。
#[derive(Clone)]
pub struct AccountIdentityService {
    pseudonymizer: AccountPseudonymizer,
}

impl std::fmt::Debug for AccountIdentityService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountIdentityService")
            .finish_non_exhaustive()
    }
}

/// 单次账号 attempt 的 wire 身份。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountScopedIdentity {
    pub prompt_cache_key: Option<String>,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub client_request_id: String,
    pub turn_id: Option<String>,
    pub window_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub installation_id: String,
}

struct IdentityValues<'a> {
    prompt_cache_key: Option<&'a str>,
    session_id: Option<&'a str>,
    thread_id: Option<&'a str>,
    client_request_id: Option<&'a str>,
    turn_id: Option<&'a str>,
    window_id: Option<&'a str>,
    parent_thread_id: Option<&'a str>,
}

impl AccountIdentityService {
    pub fn new(pseudonymizer: AccountPseudonymizer) -> Self {
        Self { pseudonymizer }
    }

    pub fn prepare_local_identity(&self, request: &mut CodexResponsesRequest) {
        if request.local_conversation_id.is_some() {
            return;
        }
        let anchor = request
            .prompt_cache_key()
            .or(request.client_session_id.as_deref())
            .or(request.client_thread_id.as_deref())
            .or(request.client_conversation_id.as_deref())
            .map(ToString::to_string)
            .or_else(|| super::resolve::derive_stable_conversation_key(request));
        request.local_conversation_id = anchor
            .as_deref()
            .map(|anchor| format!("lc_{}", self.pseudonym("local-conversation", None, anchor)));
    }

    pub fn scope(
        &self,
        request: &CodexResponsesRequest,
        account_id: &str,
        proxy_request_id: &str,
    ) -> AccountScopedIdentity {
        self.scope_values(
            account_id,
            proxy_request_id,
            IdentityValues {
                prompt_cache_key: request.prompt_cache_key(),
                session_id: request.client_session_id.as_deref(),
                thread_id: request.client_thread_id.as_deref(),
                client_request_id: request.client_request_id.as_deref(),
                turn_id: request.client_turn_id.as_deref(),
                window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
            },
        )
    }

    pub fn scope_compact(
        &self,
        request: &CodexCompactRequest,
        account_id: &str,
        proxy_request_id: &str,
    ) -> AccountScopedIdentity {
        self.scope_values(
            account_id,
            proxy_request_id,
            IdentityValues {
                prompt_cache_key: request.prompt_cache_key(),
                session_id: request.client_session_id.as_deref(),
                thread_id: request.client_thread_id.as_deref(),
                client_request_id: request.client_request_id.as_deref(),
                turn_id: request.client_turn_id.as_deref(),
                window_id: request.client_window_id.as_deref(),
                parent_thread_id: request.client_parent_thread_id.as_deref(),
            },
        )
    }

    pub fn scope_auxiliary(
        &self,
        account_id: &str,
        proxy_request_id: &str,
    ) -> AccountScopedIdentity {
        AccountScopedIdentity {
            client_request_id: format!(
                "cr_{}",
                self.pseudonym("client-request-id", Some(account_id), proxy_request_id)
            ),
            installation_id: self.pseudonymizer.installation_id(account_id),
            ..AccountScopedIdentity::default()
        }
    }

    fn scope_values(
        &self,
        account_id: &str,
        proxy_request_id: &str,
        values: IdentityValues<'_>,
    ) -> AccountScopedIdentity {
        AccountScopedIdentity {
            prompt_cache_key: self.scope_optional(
                "prompt-cache-key",
                account_id,
                values.prompt_cache_key,
            ),
            session_id: self.scope_optional("session-id", account_id, values.session_id),
            thread_id: self.scope_optional("thread-id", account_id, values.thread_id),
            client_request_id: format!(
                "cr_{}",
                self.pseudonym(
                    "client-request-id",
                    Some(account_id),
                    values.client_request_id.unwrap_or(proxy_request_id),
                )
            ),
            turn_id: self.scope_optional("turn-id", account_id, values.turn_id),
            window_id: self.scope_optional("window-id", account_id, values.window_id),
            parent_thread_id: self.scope_optional(
                "parent-thread-id",
                account_id,
                values.parent_thread_id,
            ),
            installation_id: self.pseudonymizer.installation_id(account_id),
        }
    }

    fn scope_optional(
        &self,
        domain: &str,
        account_id: &str,
        value: Option<&str>,
    ) -> Option<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("wi_{}", self.pseudonym(domain, Some(account_id), value)))
    }

    fn pseudonym(&self, domain: &str, account_id: Option<&str>, value: &str) -> String {
        self.pseudonymizer.scoped(domain, account_id, value)
    }
}
