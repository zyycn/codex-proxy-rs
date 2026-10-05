//! 账号域回调；账号归属与凭据版本由 Admin 复核，不接受插件伪造归属

use std::sync::{Arc, OnceLock, Weak};

use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{
        AdminError, PageSize, Revision,
        provider_credentials::{
            PluginAccountListQuery, PreparedCredentialCreate, PreparedCredentialRotationFacts,
            PreparedPluginAccountSave, ProviderDocument,
        },
    },
    ports::plugin_accounts::PluginAccountAccess,
};
use gateway_core::{
    account::{CredentialState, OpaqueProviderData, ProviderAccountId, ProviderAccountIdentity},
    routing::ProviderKind,
};
use gateway_plugin_sdk::{
    CallContext, ErrorCode, PluginFault,
    call::host::{
        AuthCredential, AuthGetRequest, AuthListRequest, AuthListResult, AuthRuntimeAccount,
        AuthSaveRequest, AuthSaveResult, CredentialFacts,
    },
};

use super::{
    CallbackScope,
    admin::{encode, map_admin_error, mutation_context},
    invalid,
};
use crate::RpcReply;

pub(crate) struct PluginAccountPortSlot {
    access: OnceLock<Weak<dyn PluginAccountAccess>>,
}

impl PluginAccountPortSlot {
    pub(crate) const fn new() -> Self {
        Self {
            access: OnceLock::new(),
        }
    }

    pub(crate) fn bind(&self, access: &Arc<dyn PluginAccountAccess>) -> Result<(), AdminError> {
        self.access
            .set(Arc::downgrade(access))
            .map_err(|_| AdminError::conflict("插件账号端口已经绑定"))
    }

    pub(crate) fn upgrade(&self) -> Result<Arc<dyn PluginAccountAccess>, AdminError> {
        self.access
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| AdminError::unavailable("插件账号服务暂不可用"))
    }
}

pub(super) struct PluginAccounts {
    slot: Arc<PluginAccountPortSlot>,
}

impl PluginAccounts {
    pub(super) fn new(slot: Arc<PluginAccountPortSlot>) -> Self {
        Self { slot }
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        scope: &CallbackScope,
        method: &str,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if params != serde_json::json!({}) {
            return Err(invalid());
        }
        let access = self.slot.upgrade().map_err(map_admin_error)?;
        match method {
            "host.auth.list" => {
                let request: AuthListRequest = decode(payload)?;
                let limit = PageSize::new(request.limit).map_err(|_| invalid())?;
                let cursor = request
                    .cursor
                    .map(ProviderAccountId::new)
                    .transpose()
                    .map_err(|_| invalid())?;
                let provider_kind = request
                    .provider_id
                    .map(ProviderKind::new)
                    .transpose()
                    .map_err(|_| invalid())?;
                let page = access
                    .list(PluginAccountListQuery {
                        provider_kind,
                        cursor,
                        limit,
                    })
                    .await
                    .map_err(map_admin_error)?;
                encode(&AuthListResult {
                    accounts: page.accounts.iter().map(runtime_account).collect(),
                    next_cursor: page.next_cursor.map(|cursor| cursor.as_str().to_owned()),
                })
            }
            "host.auth.get_runtime" => {
                let request: AuthGetRequest = decode(payload)?;
                let account_id =
                    ProviderAccountId::new(request.account_id).map_err(|_| invalid())?;
                let account = access
                    .get_runtime(&account_id)
                    .await
                    .map_err(map_admin_error)?;
                encode(&runtime_account(&account))
            }
            "host.auth.get" => {
                let request: AuthGetRequest = decode(payload)?;
                let account_id =
                    ProviderAccountId::new(request.account_id).map_err(|_| invalid())?;
                let credential = access
                    .get_credential(&account_id)
                    .await
                    .map_err(map_admin_error)?;
                let account = credential.account;
                encode(&AuthCredential {
                    account_id: account.id.clone(),
                    provider_id: account.provider_kind.as_str().to_owned(),
                    credential_revision: account.credential_revision.get(),
                    facts: credential_facts(account, credential.provider_material),
                })
            }
            "host.auth.save" => {
                let request: AuthSaveRequest = decode(payload)?;
                encode(&self.save(context, scope, request).await?)
            }
            _ => Err(PluginFault::new(
                ErrorCode::Unsupported,
                "account callback is unsupported",
            )),
        }
    }

    pub(super) async fn save(
        &self,
        context: &CallContext,
        scope: &CallbackScope,
        request: AuthSaveRequest,
    ) -> Result<AuthSaveResult, PluginFault> {
        let access = self.slot.upgrade().map_err(map_admin_error)?;
        let prepared = match request {
            AuthSaveRequest::Create { provider_id, facts } => {
                let provider = ProviderKind::new(provider_id).map_err(|_| invalid())?;
                PreparedPluginAccountSave::Create(prepare_create(provider, facts)?)
            }
            AuthSaveRequest::Replace {
                account_id,
                credential_revision,
                facts,
            } => {
                let account_id = ProviderAccountId::new(account_id).map_err(|_| invalid())?;
                if scope.account_id() == Some(account_id.as_str())
                    && scope
                        .credential_revision()
                        .is_some_and(|bound| bound != credential_revision)
                {
                    return Err(denied());
                }
                let account = access
                    .get_runtime(&account_id)
                    .await
                    .map_err(map_admin_error)?;
                PreparedPluginAccountSave::Replace {
                    authentication_kind: facts.authentication_kind.clone(),
                    facts: prepare_replace(
                        account.provider_kind,
                        account_id,
                        credential_revision,
                        facts,
                    )?,
                }
            }
        };
        let result = access
            .save(prepared, &mutation_context(context))
            .await
            .map_err(map_admin_error)?;
        Ok(AuthSaveResult {
            account_id: result.account_id.as_str().to_owned(),
            credential_revision: result.credential_revision.get(),
        })
    }
}

fn prepare_create(
    provider_kind: ProviderKind,
    facts: CredentialFacts,
) -> Result<PreparedCredentialCreate, PluginFault> {
    validate_facts(&facts)?;
    Ok(PreparedCredentialCreate {
        model_access: None,
        outbound_proxy: None,
        account_id: ProviderAccountId::new(format!("acct_{}", uuid::Uuid::now_v7().simple()))
            .map_err(|_| invalid())?,
        provider_kind,
        name: facts.name,
        email: facts.email,
        upstream_user_id: facts.upstream_user_id,
        upstream_account_id: facts.upstream_account_id,
        plan_type: facts.plan_type,
        authentication_kind: facts.authentication_kind,
        provider_material: ProviderDocument::new(OpaqueProviderData::new(facts.material)),
        has_refresh_token: facts.has_refresh_token,
        access_token_expires_at: credential_timestamp(facts.access_token_expires_at_ms)
            .map_err(|_| invalid())?,
        next_refresh_at: credential_timestamp(facts.next_refresh_at_ms).map_err(|_| invalid())?,
        enabled: true,
        credential_state: CredentialState::Ready,
        credential_observed_at: Utc::now(),
    })
}

fn prepare_replace(
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    credential_revision: u64,
    facts: CredentialFacts,
) -> Result<PreparedCredentialRotationFacts, PluginFault> {
    validate_facts(&facts)?;
    Ok(PreparedCredentialRotationFacts {
        account_id,
        provider_kind,
        expected_credential_revision: Revision::new(credential_revision).map_err(|_| invalid())?,
        replacement_identity: facts
            .upstream_user_id
            .clone()
            .map(|user| ProviderAccountIdentity::new(user, facts.upstream_account_id.clone())),
        name: facts.name,
        email: facts.email,
        plan_type: facts.plan_type,
        preserve_profile: false,
        preserve_credential_state: false,
        provider_material: ProviderDocument::new(OpaqueProviderData::new(facts.material)),
        has_refresh_token: facts.has_refresh_token,
        access_token_expires_at: credential_timestamp(facts.access_token_expires_at_ms)
            .map_err(|_| invalid())?,
        next_refresh_at: credential_timestamp(facts.next_refresh_at_ms).map_err(|_| invalid())?,
    })
}

fn runtime_account(account: &gateway_admin::model::accounts::AccountRecord) -> AuthRuntimeAccount {
    AuthRuntimeAccount {
        account_id: account.id.clone(),
        provider_id: account.provider_kind.as_str().to_owned(),
        credential_revision: account.credential_revision.get(),
        name: account.name.clone(),
        email: account.email.clone(),
        upstream_user_id: account.upstream_user_id.clone(),
        upstream_account_id: account.upstream_account_id.clone(),
        plan_type: account.plan_type.clone(),
        authentication_kind: account.authentication_kind.clone(),
        enabled: account.enabled,
        credential_state: account.credential_state.as_str().to_owned(),
        has_refresh_token: account.has_refresh_token,
        access_token_expires_at_ms: account
            .access_token_expires_at
            .map(|value| value.timestamp_millis()),
        next_refresh_at_ms: account
            .next_refresh_at
            .map(|value| value.timestamp_millis()),
    }
}

fn credential_facts(
    account: gateway_admin::model::accounts::AccountRecord,
    material: ProviderDocument,
) -> CredentialFacts {
    CredentialFacts {
        name: account.name,
        authentication_kind: account.authentication_kind,
        material: material.into_provider_data().into_inner(),
        email: account.email,
        upstream_user_id: account.upstream_user_id,
        upstream_account_id: account.upstream_account_id,
        plan_type: account.plan_type,
        has_refresh_token: account.has_refresh_token,
        access_token_expires_at_ms: account
            .access_token_expires_at
            .map(|value| value.timestamp_millis()),
        next_refresh_at_ms: account
            .next_refresh_at
            .map(|value| value.timestamp_millis()),
    }
}

fn decode<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, PluginFault> {
    serde_json::from_slice(payload).map_err(|_| invalid())
}

fn validate_facts(facts: &CredentialFacts) -> Result<(), PluginFault> {
    let valid_text = |value: &str, maximum: usize| {
        !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
    };
    if !valid_text(&facts.name, 200)
        || !valid_text(&facts.authentication_kind, 64)
        || [
            &facts.email,
            &facts.upstream_user_id,
            &facts.upstream_account_id,
            &facts.plan_type,
        ]
        .into_iter()
        .flatten()
        .any(|value| !valid_text(value, 1024))
        || facts.material.is_empty()
    {
        return Err(invalid());
    }
    Ok(())
}

fn credential_timestamp(value: Option<i64>) -> Result<Option<DateTime<Utc>>, PluginFault> {
    value
        .map(|value| DateTime::from_timestamp_millis(value).ok_or_else(invalid))
        .transpose()
}

fn denied() -> PluginFault {
    PluginFault::new(
        ErrorCode::PermissionDenied,
        "account callback is not authorized",
    )
}
