//! 账号资料和凭据准备结果到管理合同的转换

use super::error::provider_admin_error;
use crate::credential::CodexOAuthSecret;
use chrono::{DateTime, Utc};
use gateway_admin::model::Revision;
use gateway_admin::model::accounts::AccountRecord;
use gateway_admin::model::provider_credentials::{
    CredentialCommitGuard, PreparedCredentialCreate, PreparedCredentialRotation,
    PreparedCredentialRotationFacts, ProviderDocument,
};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::account::{
    CredentialCasUpdateParts, CredentialRevision, NewProviderAccount, OpaqueProviderData,
    ProviderAccount, ProviderAccountId,
};
use gateway_core::routing::ProviderKind;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::Value;
use std::time::SystemTime;

const PROVIDER_NAME: &str = "openai";
struct OpenAiCredentialCommitGuard {
    _guard: crate::credential::PreparedCodexCredentialRotationGuard,
}

impl CredentialCommitGuard for OpenAiCredentialCommitGuard {
    fn finish(self: Box<Self>) {}
}

pub(super) fn prepared_create(
    prepared: NewProviderAccount,
    observed_at: DateTime<Utc>,
) -> Result<PreparedCredentialCreate, ProviderAdminError> {
    let NewProviderAccount {
        account,
        model_access,
        credential,
    } = prepared;
    Ok(PreparedCredentialCreate {
        model_access,
        outbound_proxy: account.outbound_proxy().cloned(),
        account_id: account.id().clone(),
        provider_kind: account.provider().clone(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        authentication_kind: account.authentication_kind().to_owned(),
        provider_material: ProviderDocument::new(OpaqueProviderData::new(credential.into_inner())),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().map(DateTime::<Utc>::from),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        credential_state: account.credential_state(),
        credential_observed_at: observed_at,
    })
}

pub(super) fn prepared_rotation(
    prepared: crate::credential::PreparedCodexCredentialRotation,
    provider_kind: ProviderKind,
) -> Result<PreparedCredentialRotation, ProviderAdminError> {
    let (profile, credential, replacement_identity, guard) = prepared.into_parts();
    let CredentialCasUpdateParts {
        account_id,
        expected_revision,
        profile: credential_profile,
        preserve_profile,
        preserve_access_token_expiry,
        credential,
        has_refresh_token,
        access_token_expires_at,
        next_refresh_at,
        account_state: _,
    } = credential.into_parts();
    if profile != credential_profile || profile.account_id != account_id {
        return Err(provider_admin_error(ProviderAdminErrorKind::Internal));
    }
    let expected_credential_revision = Revision::new(expected_revision.get())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?;
    Ok(PreparedCredentialRotation::new(
        PreparedCredentialRotationFacts {
            account_id,
            provider_kind,
            expected_credential_revision,
            replacement_identity,
            name: profile.name,
            email: profile.email,
            plan_type: profile.plan_type,
            preserve_profile,
            preserve_credential_state: false,
            preserve_access_token_expiry,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(
                credential.into_inner(),
            )),
            has_refresh_token,
            access_token_expires_at: access_token_expires_at.map(DateTime::<Utc>::from),
            next_refresh_at: next_refresh_at.map(DateTime::<Utc>::from),
        },
        Box::new(OpenAiCredentialCommitGuard { _guard: guard }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationDocument {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

pub(super) fn rotation_secret(
    document: ProviderDocument,
) -> Result<CodexOAuthSecret, ProviderAdminError> {
    let document: RotationDocument =
        serde_json::from_value(Value::Object(document.into_provider_data().into_inner()))
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(CodexOAuthSecret {
        access_token: SecretString::from(document.access_token),
        refresh_token: document.refresh_token.map(SecretString::from),
        id_token: document.id_token.map(SecretString::from),
    })
}

pub(super) fn validate_account_record(
    account: &AccountRecord,
    provider_kind: &ProviderKind,
) -> Result<(), ProviderAdminError> {
    if &account.provider_kind != provider_kind || provider_kind.as_str() != PROVIDER_NAME {
        return Err(provider_admin_error(ProviderAdminErrorKind::Invalid));
    }
    ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(())
}

pub(super) fn account_from_record(
    account: &AccountRecord,
) -> Result<ProviderAccount, ProviderAdminError> {
    let account_id = ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    let revision = CredentialRevision::new(account.credential_revision.get())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(ProviderAccount::new(
        account_id,
        account.provider_kind.clone(),
        account.name.clone(),
        account.upstream_user_id.clone(),
        account.authentication_kind.clone(),
        revision,
        account.access_token_expires_at.map(SystemTime::from),
    )
    .with_auto_use_quota_credits(account.auto_use_quota_credits)
    .with_outbound_proxy(account.outbound_proxy.clone())
    .with_notes(account.notes.clone())
    .with_profile(
        account.email.clone(),
        account.upstream_account_id.clone(),
        account.plan_type.clone(),
    )
    .with_account_facts(
        account.enabled,
        account.credential_state,
        account.quota,
        account.last_error_reason,
        account.last_error_message.clone(),
    )
    .with_model_access(account.model_access.clone())
    .with_refresh_schedule(
        account.has_refresh_token,
        account.next_refresh_at.map(SystemTime::from),
    ))
}
pub(super) fn account_matches_record(account: &ProviderAccount, record: &AccountRecord) -> bool {
    account.id().as_str() == record.id
        && account.provider() == &record.provider_kind
        && account.upstream_user_id() == record.upstream_user_id.as_deref()
        && account.upstream_account_id() == record.upstream_account_id.as_deref()
        && account.authentication_kind() == record.authentication_kind
}
