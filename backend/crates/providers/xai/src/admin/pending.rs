//! OAuth pending 的原子存储适配与提交守卫

use super::error::{map_oauth_error, map_provider_store_error, provider_error};
use crate::SecretValue;
use crate::credential::{GrokOAuthConfig, PendingAuthorization};
use crate::transport::XAI_PROVIDER_NAME;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::model::AdminError;
use gateway_admin::model::provider_credentials::{
    AuthorizationCommitGuard, AuthorizationOwner, PendingAuthorizationMutation,
};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::account::OpaqueProviderData;
use gateway_core::provider_ports::{
    OAuthPendingBinding, OAuthPendingConsumeOutcome, OAuthPendingFlowPort,
    OAuthPendingReleaseOutcome, ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use serde::Deserialize;
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;
use std::time::Duration;

const PENDING_SCHEMA_VERSION: u64 = 3;
pub(super) const PENDING_TTL: TimeDelta = TimeDelta::minutes(30);
pub(super) const PENDING_CLAIM_TTL: Duration = Duration::from_secs(90);
const MAX_PENDING_TEXT_BYTES: usize = 512;

pub(super) struct ClaimedXaiAuthorization {
    pub(super) authorization: StoredXaiAuthorization,
    pub(super) owner_ref: String,
    pub(super) claim_ref: String,
}

pub(super) struct XaiAuthorizationCommitGuard {
    pending: Arc<dyn OAuthPendingFlowPort>,
    provider_kind: ProviderKind,
    flow: OAuthPendingBinding,
    owner: OAuthPendingBinding,
    claim: OAuthPendingBinding,
}

impl XaiAuthorizationCommitGuard {
    pub(super) fn new(
        pending: Arc<dyn OAuthPendingFlowPort>,
        provider_kind: ProviderKind,
        flow_id: &str,
        owner_ref: &str,
        claim_ref: &str,
    ) -> Result<Self, ProviderAdminError> {
        Ok(Self {
            pending,
            provider_kind,
            flow: binding(flow_id)?,
            owner: binding(owner_ref)?,
            claim: binding(claim_ref)?,
        })
    }
}

#[async_trait]
impl AuthorizationCommitGuard for XaiAuthorizationCommitGuard {
    async fn commit(self: Box<Self>) -> Result<(), AdminError> {
        match self
            .pending
            .consume_claim(&self.provider_kind, &self.flow, &self.owner, &self.claim)
            .await
            .map_err(map_pending_claim_settlement_error)?
        {
            OAuthPendingConsumeOutcome::Consumed => Ok(()),
            OAuthPendingConsumeOutcome::NotFound
            | OAuthPendingConsumeOutcome::OwnerMismatch
            | OAuthPendingConsumeOutcome::ClaimMismatch => {
                Err(AdminError::conflict("授权状态已变更，请核对账号状态"))
            }
        }
    }

    async fn abort(self: Box<Self>) -> Result<(), AdminError> {
        match self
            .pending
            .release_claim(&self.provider_kind, &self.flow, &self.owner, &self.claim)
            .await
            .map_err(map_pending_claim_settlement_error)?
        {
            OAuthPendingReleaseOutcome::Released => Ok(()),
            OAuthPendingReleaseOutcome::NotFound
            | OAuthPendingReleaseOutcome::OwnerMismatch
            | OAuthPendingReleaseOutcome::ClaimMismatch => Err(AdminError::unavailable(
                "授权流程未正常结束，请核对账号状态",
            )),
        }
    }
}
pub(super) struct StoredXaiAuthorization {
    pub(super) flow_id: String,
    pub(super) owner_ref: String,
    pub(super) expires_at: DateTime<Utc>,
    pub(super) server_state: String,
    pub(super) mutation: PendingAuthorizationMutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingDocument {
    schema_version: u64,
    flow_id: String,
    owner_ref: String,
    expires_at: DateTime<Utc>,
    server_state: String,
    mutation: Map<String, Value>,
}

pub(super) fn encode_pending(pending: &StoredXaiAuthorization) -> Map<String, Value> {
    let mut document = Map::new();
    document.insert(
        "schema_version".to_owned(),
        Value::Number(Number::from(PENDING_SCHEMA_VERSION)),
    );
    document.insert("flow_id".to_owned(), Value::String(pending.flow_id.clone()));
    document.insert(
        "owner_ref".to_owned(),
        Value::String(pending.owner_ref.clone()),
    );
    document.insert(
        "expires_at".to_owned(),
        Value::String(pending.expires_at.to_rfc3339()),
    );
    document.insert(
        "server_state".to_owned(),
        Value::String(pending.server_state.clone()),
    );
    document.insert(
        "mutation".to_owned(),
        Value::Object(pending.mutation.to_storage_v1()),
    );
    document
}

pub(super) fn decode_pending(
    payload: OpaqueProviderData,
    oauth_config: &GrokOAuthConfig,
) -> Result<StoredXaiAuthorization, ProviderAdminError> {
    let document: PendingDocument = serde_json::from_value(Value::Object(payload.into_inner()))
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    if document.schema_version != PENDING_SCHEMA_VERSION
        || !valid_pending_text(&document.flow_id)
        || !valid_pending_text(&document.owner_ref)
        || document.expires_at <= Utc::now()
    {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    let mutation = PendingAuthorizationMutation::from_storage_v1(Value::Object(document.mutation))
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    if mutation.provider_kind().as_str() != XAI_PROVIDER_NAME
        || owner_ref(mutation.owner_binding().owner()) != document.owner_ref
    {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    PendingAuthorization::from_server_state(
        oauth_config,
        &SecretValue::new(document.server_state.clone()),
    )
    .map_err(map_oauth_error)?;
    Ok(StoredXaiAuthorization {
        flow_id: document.flow_id,
        owner_ref: document.owner_ref,
        expires_at: document.expires_at,
        server_state: document.server_state,
        mutation,
    })
}

pub(super) fn random_flow_id() -> Result<String, ProviderAdminError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| provider_error(ProviderAdminErrorKind::Unavailable))?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

pub(super) fn owner_ref(owner: &AuthorizationOwner) -> String {
    let mut digest = Sha256::new();
    match owner {
        AuthorizationOwner::AdminSession { admin_user_id } => {
            digest.update(b"admin-session\0");
            digest.update(admin_user_id.as_bytes());
        }
        AuthorizationOwner::AdminApiKey => digest.update(b"admin-api-key"),
        AuthorizationOwner::System => digest.update(b"system"),
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

pub(super) fn binding(value: &str) -> Result<OAuthPendingBinding, ProviderAdminError> {
    OAuthPendingBinding::try_new(value.to_owned()).map_err(map_provider_store_error)
}

fn valid_pending_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PENDING_TEXT_BYTES
        && !value.chars().any(char::is_control)
}
fn map_pending_claim_settlement_error(error: ProviderStoreError) -> AdminError {
    match error.kind() {
        ProviderStoreErrorKind::InvalidData => {
            AdminError::internal("xAI OAuth pending claim is invalid")
        }
        ProviderStoreErrorKind::Conflict => AdminError::conflict("授权状态冲突，请核对账号状态"),
        ProviderStoreErrorKind::Unavailable => AdminError::unavailable("授权状态存储暂不可用"),
    }
}
