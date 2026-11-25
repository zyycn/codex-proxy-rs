//! OAuth pending 的原子存储适配与提交守卫

use crate::credential::{
    CodexOAuthPendingClaimOutcome, CodexOAuthPendingStore, CodexOAuthPendingStoreError,
    CodexPendingAuthorization, StoredCodexPendingAuthorization,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::model::provider_credentials::PendingAuthorizationMutation;
use gateway_core::account::OpaqueProviderData;
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingClaimOutcome, OAuthPendingConsumeOutcome,
    OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingReleaseOutcome, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::{Map, Number, Value};
use std::sync::Arc;

const PENDING_DOCUMENT_SCHEMA_VERSION: u64 = 3;
/// 将 Provider-owned PKCE/OIDC 状态保存到 Store 提供的 Redis 原子端口
pub(crate) struct OpenAiOAuthPendingStore {
    port: Arc<dyn OAuthPendingFlowPort>,
    provider_kind: ProviderKind,
}

impl OpenAiOAuthPendingStore {
    pub(crate) const fn new(
        port: Arc<dyn OAuthPendingFlowPort>,
        provider_kind: ProviderKind,
    ) -> Self {
        Self {
            port,
            provider_kind,
        }
    }
}

#[async_trait]
impl CodexOAuthPendingStore for OpenAiOAuthPendingStore {
    async fn create(
        &self,
        pending: &CodexPendingAuthorization,
    ) -> Result<(), CodexOAuthPendingStoreError> {
        let now = Utc::now();
        let ttl = (pending.expires_at() - now)
            .to_std()
            .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
        let flow = NewOAuthPendingFlow::try_new(
            self.provider_kind.clone(),
            binding(pending.flow_id())?,
            binding(pending.owner_ref())?,
            ttl,
            OpaqueProviderData::new(encode_pending(pending)),
        )
        .map_err(map_pending_store_error)?;
        match self
            .port
            .put_if_absent(flow)
            .await
            .map_err(map_pending_store_error)?
        {
            OAuthPendingPutOutcome::Stored => Ok(()),
            OAuthPendingPutOutcome::AlreadyExists => Err(CodexOAuthPendingStoreError::Conflict),
        }
    }

    async fn claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
        claim_ttl: std::time::Duration,
    ) -> Result<CodexOAuthPendingClaimOutcome, CodexOAuthPendingStoreError> {
        let flow = binding(flow_id)?;
        let owner = binding(owner_ref)?;
        let claim = binding(claim_ref)?;
        let outcome = self
            .port
            .claim_if_owner(&self.provider_kind, &flow, &owner, &claim, claim_ttl)
            .await
            .map_err(map_pending_store_error)?;
        match outcome {
            OAuthPendingClaimOutcome::Claimed(payload) => match decode_pending(payload) {
                Ok(pending) => Ok(CodexOAuthPendingClaimOutcome::Claimed(Box::new(pending))),
                Err(error) => match self
                    .port
                    .release_claim(&self.provider_kind, &flow, &owner, &claim)
                    .await
                    .map_err(map_pending_store_error)?
                {
                    OAuthPendingReleaseOutcome::Released => Err(error),
                    OAuthPendingReleaseOutcome::NotFound
                    | OAuthPendingReleaseOutcome::OwnerMismatch
                    | OAuthPendingReleaseOutcome::ClaimMismatch => {
                        Err(CodexOAuthPendingStoreError::Unavailable)
                    }
                },
            },
            OAuthPendingClaimOutcome::NotFound | OAuthPendingClaimOutcome::OwnerMismatch => {
                Ok(CodexOAuthPendingClaimOutcome::NotFound)
            }
            OAuthPendingClaimOutcome::InProgress => Ok(CodexOAuthPendingClaimOutcome::InProgress),
        }
    }

    async fn release_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        let flow = binding(flow_id)?;
        let owner = binding(owner_ref)?;
        let claim = binding(claim_ref)?;
        match self
            .port
            .release_claim(&self.provider_kind, &flow, &owner, &claim)
            .await
            .map_err(map_pending_store_error)?
        {
            OAuthPendingReleaseOutcome::Released => Ok(true),
            OAuthPendingReleaseOutcome::NotFound
            | OAuthPendingReleaseOutcome::OwnerMismatch
            | OAuthPendingReleaseOutcome::ClaimMismatch => Ok(false),
        }
    }

    async fn consume_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        let flow = binding(flow_id)?;
        let owner = binding(owner_ref)?;
        let claim = binding(claim_ref)?;
        match self
            .port
            .consume_claim(&self.provider_kind, &flow, &owner, &claim)
            .await
            .map_err(map_pending_store_error)?
        {
            OAuthPendingConsumeOutcome::Consumed => Ok(true),
            OAuthPendingConsumeOutcome::NotFound
            | OAuthPendingConsumeOutcome::OwnerMismatch
            | OAuthPendingConsumeOutcome::ClaimMismatch => Ok(false),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingDocument {
    flow_id: String,
    owner_ref: String,
    started_request_ref: String,
    name: String,
    expires_at: DateTime<Utc>,
    state: String,
    nonce: String,
    code_verifier: String,
    installation_id: String,
    reauthorization_account_id: Option<String>,
    mutation: Map<String, Value>,
}

fn encode_pending(pending: &CodexPendingAuthorization) -> Map<String, Value> {
    let mut document = Map::new();
    document.insert(
        "flow_id".to_owned(),
        Value::String(pending.flow_id().to_owned()),
    );
    document.insert(
        "owner_ref".to_owned(),
        Value::String(pending.owner_ref().to_owned()),
    );
    document.insert(
        "started_request_ref".to_owned(),
        Value::String(pending.started_request_ref().to_owned()),
    );
    document.insert("name".to_owned(), Value::String(pending.name().to_owned()));
    document.insert(
        "expires_at".to_owned(),
        Value::String(pending.expires_at().to_rfc3339()),
    );
    document.insert(
        "state".to_owned(),
        Value::String(pending.state().expose_secret().to_owned()),
    );
    document.insert(
        "nonce".to_owned(),
        Value::String(pending.nonce().expose_secret().to_owned()),
    );
    document.insert(
        "code_verifier".to_owned(),
        Value::String(pending.code_verifier().expose_secret().to_owned()),
    );
    document.insert(
        "installation_id".to_owned(),
        Value::String(pending.installation_id().to_owned()),
    );
    document.insert(
        "reauthorization_account_id".to_owned(),
        pending
            .reauthorization()
            .map_or(Value::Null, |target| Value::String(target.to_string())),
    );
    document.insert(
        "mutation".to_owned(),
        Value::Object(encode_mutation(pending.mutation())),
    );
    document
}

fn encode_mutation(mutation: &PendingAuthorizationMutation) -> Map<String, Value> {
    let mut document = mutation.to_storage_v1();
    document.insert(
        "schema_version".to_owned(),
        Value::Number(Number::from(PENDING_DOCUMENT_SCHEMA_VERSION)),
    );
    document
}

fn decode_pending(
    payload: OpaqueProviderData,
) -> Result<CodexPendingAuthorization, CodexOAuthPendingStoreError> {
    let document: PendingDocument = serde_json::from_value(Value::Object(payload.into_inner()))
        .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
    CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
        flow_id: document.flow_id,
        owner_ref: document.owner_ref,
        started_request_ref: document.started_request_ref,
        name: document.name,
        expires_at: document.expires_at,
        state: SecretString::from(document.state),
        nonce: SecretString::from(document.nonce),
        code_verifier: SecretString::from(document.code_verifier),
        installation_id: document.installation_id,
        reauthorization_account_id: document.reauthorization_account_id,
        mutation: decode_mutation(document.mutation)?,
    })
}

fn decode_mutation(
    mut document: Map<String, Value>,
) -> Result<PendingAuthorizationMutation, CodexOAuthPendingStoreError> {
    if document
        .remove("schema_version")
        .and_then(|value| value.as_u64())
        != Some(PENDING_DOCUMENT_SCHEMA_VERSION)
    {
        return Err(CodexOAuthPendingStoreError::InvalidValue);
    }
    PendingAuthorizationMutation::from_storage_v1(Value::Object(document))
        .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)
}

pub(super) fn binding(value: &str) -> Result<OAuthPendingBinding, CodexOAuthPendingStoreError> {
    OAuthPendingBinding::try_new(value.to_owned()).map_err(map_pending_store_error)
}
fn map_pending_store_error(error: ProviderStoreError) -> CodexOAuthPendingStoreError {
    match error.kind() {
        ProviderStoreErrorKind::InvalidData => CodexOAuthPendingStoreError::InvalidValue,
        ProviderStoreErrorKind::Conflict => CodexOAuthPendingStoreError::Conflict,
        ProviderStoreErrorKind::Unavailable => CodexOAuthPendingStoreError::Unavailable,
    }
}
