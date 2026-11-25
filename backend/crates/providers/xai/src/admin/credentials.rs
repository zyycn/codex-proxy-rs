//! 账号资料和凭据准备结果到管理合同的转换

use super::error::{map_repository_error, provider_error};
use crate::credential::{
    GrokAccountProfile, GrokCredentialAdmin, GrokOAuthSecret, PreparedGrokCredentialRotation,
    PreparedGrokCredentialRotationGuard, RotateManagedGrokCredential, VerifiedTokenSet,
};
use crate::transport::XAI_PROVIDER_NAME;
use chrono::{DateTime, Utc};
use gateway_admin::model::Revision;
use gateway_admin::model::accounts::AccountRecord;
use gateway_admin::model::provider_credentials::{
    CredentialCommitGuard, PreparedCredentialCreate, PreparedCredentialRotation,
    PreparedCredentialRotationFacts, ProviderDocument,
};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::account::{
    CredentialCasUpdateParts, CredentialRevision, LoadedCredential, NewProviderAccount,
    OpaqueProviderData, ProviderAccount, ProviderAccountId,
};
use gateway_core::routing::ProviderKind;
use serde::Deserialize;
use std::time::SystemTime;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RotationDocument {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) id_token: Option<String>,
    pub(super) scope: String,
    pub(super) expires_at: DateTime<Utc>,
}

struct XaiCredentialCommitGuard {
    _guard: PreparedGrokCredentialRotationGuard,
}

impl CredentialCommitGuard for XaiCredentialCommitGuard {
    fn finish(self: Box<Self>) {}
}

pub(super) fn verified_rotation(
    current: LoadedCredential,
    tokens: VerifiedTokenSet,
) -> Result<PreparedGrokCredentialRotation, ProviderAdminError> {
    let expires_in = tokens
        .expires_in()
        .ok_or_else(|| provider_error(ProviderAdminErrorKind::Invalid))?;
    let refresh_token = tokens
        .refresh_token()
        .cloned()
        .ok_or_else(|| provider_error(ProviderAdminErrorKind::Invalid))?;
    let access_token_expires_at = SystemTime::now()
        .checked_add(expires_in)
        .ok_or_else(|| provider_error(ProviderAdminErrorKind::Invalid))?;
    GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            secret: GrokOAuthSecret {
                access_token: tokens.access_token().clone(),
                refresh_token,
                id_token: tokens.id_token().cloned(),
                scope: tokens.scope().to_owned(),
            },
            verified_account: GrokAccountProfile {
                subject: tokens.evidence().subject().to_owned(),
                email: current.account.email().map(str::to_owned),
                upstream_account_id: current.account.upstream_account_id().map(str::to_owned),
                plan_type: current.account.plan_type().map(str::to_owned),
                access_token_expires_at: access_token_expires_at.into(),
                refresh_token_expires_at: None,
            },
            current,
        })
        .map_err(map_repository_error)
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
        next_refresh_at: account.next_refresh_at().map(Into::into),
        enabled: account.enabled(),
        outbound_proxy: account.outbound_proxy().cloned(),
        credential_state: account.credential_state(),
        credential_observed_at: observed_at,
    })
}

pub(super) fn prepared_rotation(
    prepared: PreparedGrokCredentialRotation,
    provider_kind: ProviderKind,
) -> Result<PreparedCredentialRotation, ProviderAdminError> {
    let (profile, credential, guard) = prepared.into_parts();
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
        return Err(provider_error(ProviderAdminErrorKind::Internal));
    }
    Ok(PreparedCredentialRotation::new(
        PreparedCredentialRotationFacts {
            account_id,
            provider_kind,
            expected_credential_revision: Revision::new(expected_revision.get())
                .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?,
            replacement_identity: None,
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
            next_refresh_at: next_refresh_at.map(Into::into),
        },
        Box::new(XaiCredentialCommitGuard { _guard: guard }),
    ))
}

pub(super) fn validate_account_record(
    account: &AccountRecord,
    provider_kind: &ProviderKind,
) -> Result<(), ProviderAdminError> {
    if &account.provider_kind != provider_kind || provider_kind.as_str() != XAI_PROVIDER_NAME {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    Ok(())
}
pub(super) fn account_from_record(
    account: &AccountRecord,
) -> Result<ProviderAccount, ProviderAdminError> {
    let account_id = ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    let revision = CredentialRevision::new(account.credential_revision.get())
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    Ok(ProviderAccount::new(
        account_id,
        account.provider_kind.clone(),
        account.name.clone(),
        account.upstream_user_id.clone(),
        account.authentication_kind.clone(),
        revision,
        account.access_token_expires_at.map(SystemTime::from),
    )
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
        account.next_refresh_at.map(Into::into),
    ))
}

pub(super) fn account_matches_record(account: &ProviderAccount, record: &AccountRecord) -> bool {
    account.id().as_str() == record.id
        && account.provider() == &record.provider_kind
        && account.upstream_user_id() == record.upstream_user_id.as_deref()
        && account.upstream_account_id() == record.upstream_account_id.as_deref()
        && account.authentication_kind() == record.authentication_kind
}
