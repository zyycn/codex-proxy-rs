//! 绑定管理员与 OAuth 注册的短期授权流程，由 Admin 在账号事务提交后消费

use async_trait::async_trait;
use chrono::Utc;
use gateway_admin::{
    model::{
        AdminError,
        provider_credentials::{
            AuthorizationCommitGuard, AuthorizationMutationTarget, AuthorizationOwner,
            AuthorizationOwnerBinding, AuthorizationStarted, CompleteAuthorization,
            PendingAuthorizationMutation, PreparedAuthorizationCommit,
        },
    },
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind as Kind},
};
use gateway_core::{
    account::OpaqueProviderData,
    provider_ports::{
        NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingClaimOutcome,
        OAuthPendingConsumeOutcome, OAuthPendingFlowPort, OAuthPendingPutOutcome,
        OAuthPendingReleaseOutcome,
    },
    routing::ProviderKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

use crate::credential::{
    CredentialService,
    oauth::{GoogleOAuth, error, random_value},
};

const FLOW_TTL: Duration = Duration::from_secs(600);

#[derive(Serialize, Deserialize)]
struct Pending {
    registration: String,
    state: String,
    verifier: String,
    mutation: Value,
}

struct Claim {
    store: Arc<dyn OAuthPendingFlowPort>,
    kind: ProviderKind,
    flow: OAuthPendingBinding,
    owner: OAuthPendingBinding,
    claim: OAuthPendingBinding,
}

fn binding(value: String) -> Result<OAuthPendingBinding, ProviderAdminError> {
    OAuthPendingBinding::try_new(value).map_err(|_| error(Kind::Invalid))
}

fn owner_binding(
    owner: &AuthorizationOwnerBinding,
) -> Result<OAuthPendingBinding, ProviderAdminError> {
    binding(match owner.owner() {
        AuthorizationOwner::AdminSession { admin_user_id } => {
            format!("admin-session/{admin_user_id}")
        }
        AuthorizationOwner::System => "system".into(),
        AuthorizationOwner::AdminApiKey => "admin-api-key".into(),
    })
}

#[async_trait]
impl AuthorizationCommitGuard for Claim {
    async fn commit(self: Box<Self>) -> Result<(), AdminError> {
        match self
            .store
            .consume_claim(&self.kind, &self.flow, &self.owner, &self.claim)
            .await
        {
            Ok(OAuthPendingConsumeOutcome::Consumed) => Ok(()),
            Ok(_) => Err(AdminError::conflict("授权状态已变更，请核对账号状态")),
            Err(_) => Err(AdminError::unavailable("无法结算授权流程")),
        }
    }
    async fn abort(self: Box<Self>) -> Result<(), AdminError> {
        match self
            .store
            .release_claim(&self.kind, &self.flow, &self.owner, &self.claim)
            .await
        {
            Ok(OAuthPendingReleaseOutcome::Released) => Ok(()),
            _ => Err(AdminError::unavailable("无法释放授权流程")),
        }
    }
}

pub(crate) struct AuthorizationService {
    pub(crate) kind: ProviderKind,
    pub(crate) credentials: Arc<CredentialService>,
    pub(crate) oauth: Arc<GoogleOAuth>,
    pub(crate) store: Arc<dyn OAuthPendingFlowPort>,
}

impl AuthorizationService {
    pub(crate) async fn start(
        &self,
        mutation: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        let oauth = &self.oauth;
        if mutation.provider_kind() != &self.kind {
            return Err(error(Kind::Invalid));
        }
        if let AuthorizationMutationTarget::Reauthorize { account_id } = mutation.target() {
            self.credentials.current(account_id).await?;
        }
        let flow_id = random_value()?;
        let state = random_value()?;
        let verifier = random_value()?;
        let authorization_url = oauth.authorize_url(&state, &verifier)?;
        let stored = Pending {
            state,
            verifier,
            registration: oauth.registration(),
            mutation: Value::Object(mutation.to_storage_v1()),
        };
        let Value::Object(payload) =
            serde_json::to_value(stored).map_err(|_| error(Kind::Internal))?
        else {
            return Err(error(Kind::Internal));
        };
        let record = NewOAuthPendingFlow::try_new(
            self.kind.clone(),
            binding(flow_id.clone())?,
            owner_binding(mutation.owner_binding())?,
            FLOW_TTL,
            OpaqueProviderData::new(payload),
        )
        .map_err(|_| error(Kind::Internal))?;
        let expires_at = Utc::now() + FLOW_TTL;
        match self.store.clone().put_if_absent(record).await {
            Ok(OAuthPendingPutOutcome::Stored) => Ok(AuthorizationStarted {
                flow_id,
                authorization_url,
                expires_at,
            }),
            _ => Err(error(Kind::Unavailable)),
        }
    }

    pub(crate) async fn complete(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        let owner = AuthorizationOwnerBinding::from_context(&command.context);
        let guard = Box::new(Claim {
            store: self.store.clone(),
            kind: self.kind.clone(),
            flow: binding(command.flow_id)?,
            owner: owner_binding(&owner)?,
            claim: binding(random_value()?)?,
        });
        let payload = match guard
            .store
            .claim_if_owner(
                &guard.kind,
                &guard.flow,
                &guard.owner,
                &guard.claim,
                Duration::from_secs(120),
            )
            .await
            .map_err(|_| error(Kind::Unavailable))?
        {
            OAuthPendingClaimOutcome::Claimed(payload) => payload,
            OAuthPendingClaimOutcome::InProgress => return Err(error(Kind::Conflict)),
            OAuthPendingClaimOutcome::NotFound | OAuthPendingClaimOutcome::OwnerMismatch => {
                return Err(error(Kind::NotFound));
            }
        };
        let result = async {
            let pending: Pending = serde_json::from_value(Value::Object(payload.into_inner()))
                .map_err(|_| error(Kind::Internal))?;
            let mutation = PendingAuthorizationMutation::from_storage_v1(pending.mutation)
                .map_err(|_| error(Kind::Internal))?;
            let oauth = &self.oauth;
            if mutation.provider_kind() != &self.kind
                || mutation.owner_binding().owner() != owner.owner()
                || pending.registration != oauth.registration()
            {
                return Err(error(Kind::Conflict));
            }
            let code = oauth.callback_code(&command.callback_url, &pending.state)?;
            let profile = self.credentials.request_profile().await?;
            let grant = oauth
                .exchange(
                    &code,
                    &pending.verifier,
                    mutation.outbound_proxy(),
                    &profile,
                )
                .await?;
            let credential = self.credentials.prepare_grant(&mutation, grant).await?;
            Ok(PreparedAuthorizationCommit::new(mutation, credential))
        }
        .await;
        match result {
            Ok(prepared) => Ok(prepared.with_authorization_guard(guard)),
            Err(e) => {
                guard.abort().await.map_err(|_| error(Kind::Unavailable))?;
                Err(e)
            }
        }
    }
}
