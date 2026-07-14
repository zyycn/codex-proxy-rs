//! 客户端身份、本地会话索引与账号作用域 wire 身份隔离。

use serde_json::{Map, Value};

use crate::{
    infra::identity::AccountPseudonymizer,
    upstream::openai::protocol::responses::CodexResponsesRequest,
};

const TURN_STATE_METADATA_KEY: &str = "x-codex-turn-state";
const TURN_METADATA_KEY: &str = "x-codex-turn-metadata";

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
pub(in crate::dispatch) struct AccountScopedIdentity {
    pub installation_id: String,
}

/// 单个请求内账号绑定 opaque 状态的 owner。
pub(in crate::dispatch) struct AccountIdentityScope {
    turn_state_account_id: Option<String>,
}

impl AccountIdentityScope {
    pub(in crate::dispatch) fn new(preferred_account_id: Option<String>) -> Self {
        Self {
            turn_state_account_id: preferred_account_id,
        }
    }

    fn preserves_turn_state(&mut self, account_id: &str, has_turn_state: bool) -> bool {
        if !has_turn_state {
            return false;
        }
        let owner = self
            .turn_state_account_id
            .get_or_insert_with(|| account_id.to_string());
        owner == account_id
    }
}

/// 已按当前账号重建 wire 身份、可安全交给上游 transport 的请求。
pub(in crate::dispatch) struct AccountScopedRequest {
    request: CodexResponsesRequest,
    identity: AccountScopedIdentity,
}

impl AccountScopedRequest {
    pub(in crate::dispatch) fn request(&self) -> &CodexResponsesRequest {
        &self.request
    }

    pub(in crate::dispatch) fn identity(&self) -> &AccountScopedIdentity {
        &self.identity
    }

    pub(in crate::dispatch) fn into_request(self) -> CodexResponsesRequest {
        self.request
    }
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

    /// 从请求语义为单次账号 attempt 重建完整 wire 身份。
    pub(in crate::dispatch) fn scope_request(
        &self,
        scope: &mut AccountIdentityScope,
        mut request: CodexResponsesRequest,
        account_id: &str,
    ) -> AccountScopedRequest {
        let identity = AccountScopedIdentity {
            installation_id: self.pseudonymizer.installation_id(account_id),
        };
        let client_metadata_turn_state = client_metadata_string(&request, TURN_STATE_METADATA_KEY);
        let preserve_turn_state = scope.preserves_turn_state(
            account_id,
            request.turn_state.is_some() || client_metadata_turn_state.is_some(),
        );
        let turn_state = if preserve_turn_state {
            request.turn_state.clone()
        } else {
            None
        };
        let client_metadata_turn_state = if preserve_turn_state {
            client_metadata_turn_state
        } else {
            None
        };
        let turn_metadata = request
            .turn_metadata
            .as_deref()
            .map(|raw| Self::scope_turn_metadata(raw, &identity));
        let client_metadata_turn_metadata = client_metadata_string(&request, TURN_METADATA_KEY)
            .map(|raw| Self::scope_turn_metadata(&raw, &identity));

        apply_identity_to_request(
            &mut request,
            &identity,
            turn_state.as_deref(),
            turn_metadata.as_deref(),
            client_metadata_turn_state.as_deref(),
            client_metadata_turn_metadata.as_deref(),
        );
        request.turn_state = turn_state;
        request.turn_metadata = turn_metadata;

        AccountScopedRequest { request, identity }
    }

    pub(in crate::dispatch) fn scope_auxiliary(&self, account_id: &str) -> AccountScopedIdentity {
        AccountScopedIdentity {
            installation_id: self.pseudonymizer.installation_id(account_id),
        }
    }

    fn scope_turn_metadata(raw: &str, identity: &AccountScopedIdentity) -> String {
        let Ok(Value::Object(mut metadata)) = serde_json::from_str::<Value>(raw) else {
            return raw.to_string();
        };

        let contains_installation_identity = [
            "installation_id",
            "installationId",
            "x-codex-installation-id",
        ]
        .into_iter()
        .any(|key| metadata.contains_key(key));
        if !contains_installation_identity {
            return raw.to_string();
        }

        for key in [
            "installation_id",
            "installationId",
            "x-codex-installation-id",
        ] {
            replace_existing_metadata_field(&mut metadata, key, Some(&identity.installation_id));
        }

        serde_json::to_string(&metadata).unwrap_or_else(|_| raw.to_string())
    }

    fn pseudonym(&self, domain: &str, account_id: Option<&str>, value: &str) -> String {
        self.pseudonymizer.scoped(domain, account_id, value)
    }
}

fn client_metadata_string(request: &CodexResponsesRequest, key: &str) -> Option<String> {
    request
        .client_metadata()?
        .as_object()?
        .get(key)?
        .as_str()
        .map(ToString::to_string)
}

fn apply_identity_to_request(
    request: &mut CodexResponsesRequest,
    identity: &AccountScopedIdentity,
    turn_state: Option<&str>,
    turn_metadata: Option<&str>,
    client_metadata_turn_state: Option<&str>,
    client_metadata_turn_metadata: Option<&str>,
) {
    for (key, value) in [
        (
            "x-codex-installation-id",
            Some(identity.installation_id.as_str()),
        ),
        ("installation_id", Some(identity.installation_id.as_str())),
        ("installationId", Some(identity.installation_id.as_str())),
        ("turnState", turn_state),
        ("x-codex-turn-state", turn_state),
        ("turnMetadata", turn_metadata),
        ("x-codex-turn-metadata", turn_metadata),
    ] {
        request.replace_existing_identity_field(key, value);
    }

    let client_metadata = scoped_client_metadata(
        request.client_metadata(),
        identity,
        client_metadata_turn_state,
        client_metadata_turn_metadata,
    );
    request.set_client_metadata(client_metadata);
}

fn scoped_client_metadata(
    client_metadata: Option<&Value>,
    identity: &AccountScopedIdentity,
    turn_state: Option<&str>,
    turn_metadata: Option<&str>,
) -> Option<Value> {
    let client_metadata = client_metadata?;
    let Value::Object(mut metadata) = client_metadata.clone() else {
        return Some(client_metadata.clone());
    };

    for (key, value) in [
        (
            "x-codex-installation-id",
            Some(identity.installation_id.as_str()),
        ),
        (TURN_STATE_METADATA_KEY, turn_state),
        (TURN_METADATA_KEY, turn_metadata),
    ] {
        replace_metadata_field(&mut metadata, key, value);
    }
    replace_existing_metadata_field(
        &mut metadata,
        "installation_id",
        Some(&identity.installation_id),
    );
    replace_existing_metadata_field(
        &mut metadata,
        "installationId",
        Some(&identity.installation_id),
    );

    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

fn replace_existing_metadata_field(
    metadata: &mut Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if metadata.contains_key(key) {
        replace_metadata_field(metadata, key, value);
    }
}

fn replace_metadata_field(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => {
            metadata.insert(key.to_string(), Value::String(value.to_string()));
        }
        None => {
            metadata.remove(key);
        }
    }
}
