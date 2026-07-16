//! 客户端身份、本地会话索引与账号作用域 wire 身份隔离。

use serde_json::{Map, Value};

use crate::{
    dispatch::controllers::history::{CrossAccountReplay, sanitize_cross_account_input},
    infra::identity::AccountPseudonymizer,
    upstream::openai::protocol::responses::CodexResponsesRequest,
};

const TURN_STATE_METADATA_KEY: &str = "x-codex-turn-state";
const TURN_METADATA_KEY: &str = "x-codex-turn-metadata";
const INSTALLATION_IDENTITY_KEYS: [&str; 3] = [
    "installation_id",
    "installationId",
    "x-codex-installation-id",
];
const CROSS_ACCOUNT_IDENTITY_KEYS: [&str; 26] = [
    "authorization",
    "Authorization",
    "cookie",
    "Cookie",
    "chatgpt-account-id",
    "chatgpt_account_id",
    "chatgptAccountId",
    "account_id",
    "accountId",
    "user_id",
    "userId",
    "chatgpt_user_id",
    "chatgptUserId",
    "access_token",
    "accessToken",
    "session_token",
    "sessionToken",
    "refresh_token",
    "refreshToken",
    "id_token",
    "idToken",
    "token",
    "cookies",
    "cookie_header",
    "cookieHeader",
    "cf_clearance",
];
const CROSS_ACCOUNT_OPAQUE_STATE_KEYS: [&str; 8] = [
    "turnState",
    "turn_state",
    "x-codex-turn-state",
    "previous_response_id",
    "previousResponseId",
    "response_id",
    "responseId",
    "conversation",
];

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
    account_bound_state_owner_id: Option<String>,
}

impl AccountIdentityScope {
    pub(in crate::dispatch) fn new(preferred_account_id: Option<String>) -> Self {
        Self {
            account_bound_state_owner_id: preferred_account_id,
        }
    }

    fn bind_and_is_cross_account(&mut self, account_id: &str) -> bool {
        let owner = self
            .account_bound_state_owner_id
            .get_or_insert_with(|| account_id.to_string());
        owner != account_id
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
        cross_account_replay: Option<&CrossAccountReplay>,
    ) -> AccountScopedRequest {
        if let Some(replay) = cross_account_replay {
            replay.validate_target(account_id);
        }
        let cross_account =
            cross_account_replay.is_some() || scope.bind_and_is_cross_account(account_id);
        let identity = AccountScopedIdentity {
            installation_id: self.pseudonymizer.installation_id(account_id),
        };
        let client_metadata_turn_state = client_metadata_string(&request, TURN_STATE_METADATA_KEY);
        let preserve_turn_state = !cross_account
            && (request.turn_state.is_some() || client_metadata_turn_state.is_some());
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
            .and_then(|raw| Self::scope_turn_metadata(raw, &identity, cross_account));
        let client_metadata_turn_metadata = client_metadata_string(&request, TURN_METADATA_KEY)
            .and_then(|raw| Self::scope_turn_metadata(&raw, &identity, cross_account));

        if cross_account {
            sanitize_cross_account_input(&mut request);
            remove_cross_account_request_state(&mut request);
        }

        apply_identity_to_request(
            &mut request,
            &identity,
            turn_state.as_deref(),
            turn_metadata.as_deref(),
            client_metadata_turn_state.as_deref(),
            client_metadata_turn_metadata.as_deref(),
            cross_account,
        );
        request.turn_state = turn_state;
        request.turn_metadata = turn_metadata;

        AccountScopedRequest { request, identity }
    }

    fn scope_turn_metadata(
        raw: &str,
        identity: &AccountScopedIdentity,
        cross_account: bool,
    ) -> Option<String> {
        let Ok(Value::Object(mut metadata)) = serde_json::from_str::<Value>(raw) else {
            return (!cross_account).then(|| raw.to_string());
        };

        if cross_account {
            remove_cross_account_metadata_state(&mut metadata);
        } else if !INSTALLATION_IDENTITY_KEYS
            .into_iter()
            .any(|key| metadata.contains_key(key))
        {
            return Some(raw.to_string());
        }

        for key in INSTALLATION_IDENTITY_KEYS {
            replace_existing_metadata_field(&mut metadata, key, Some(&identity.installation_id));
        }

        match serde_json::to_string(&metadata) {
            Ok(metadata) => Some(metadata),
            Err(_) if cross_account => None,
            Err(_) => Some(raw.to_string()),
        }
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
    cross_account: bool,
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
        cross_account,
    );
    request.set_client_metadata(client_metadata);
}

fn scoped_client_metadata(
    client_metadata: Option<&Value>,
    identity: &AccountScopedIdentity,
    turn_state: Option<&str>,
    turn_metadata: Option<&str>,
    cross_account: bool,
) -> Option<Value> {
    let client_metadata = client_metadata?;
    let Value::Object(mut metadata) = client_metadata.clone() else {
        return (!cross_account).then(|| client_metadata.clone());
    };

    if cross_account {
        remove_cross_account_metadata_state(&mut metadata);
    }

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

fn remove_cross_account_request_state(request: &mut CodexResponsesRequest) {
    for key in CROSS_ACCOUNT_IDENTITY_KEYS
        .into_iter()
        .chain(CROSS_ACCOUNT_OPAQUE_STATE_KEYS)
    {
        request.replace_existing_identity_field(key, None);
    }
    for key in ["turn_metadata", TURN_METADATA_KEY] {
        request.replace_existing_identity_field(key, None);
    }
}

fn remove_cross_account_metadata_state(metadata: &mut Map<String, Value>) {
    for key in CROSS_ACCOUNT_IDENTITY_KEYS
        .into_iter()
        .chain(CROSS_ACCOUNT_OPAQUE_STATE_KEYS)
        .chain(["turnMetadata", "turn_metadata", TURN_METADATA_KEY])
    {
        metadata.remove(key);
    }
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
